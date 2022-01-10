use crate::address_map::Stopping;
use crate::cfd_actors::apply_event;
use crate::cfd_actors::load_cfd;
use crate::maker_inc_connections;
use crate::maker_inc_connections::TakerMessage;
use crate::model::cfd::Dlc;
use crate::model::cfd::Order;
use crate::model::cfd::OrderId;
use crate::model::cfd::Role;
use crate::model::cfd::SetupCompleted;
use crate::model::Identity;
use crate::model::Usd;
use crate::oracle::Announcement;
use crate::process_manager;
use crate::send_async_safe::SendAsyncSafe;
use crate::setup_contract;
use crate::tokio_ext::spawn_fallible;
use crate::wallet;
use crate::wire;
use crate::wire::MakerToTaker;
use crate::wire::SetupMsg;
use crate::xtra_ext::LogFailure;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use futures::channel::mpsc;
use futures::channel::mpsc::UnboundedSender;
use futures::future;
use futures::SinkExt;
use maia::secp256k1_zkp::schnorrsig;
use xtra::prelude::MessageChannel;
use xtra::Address;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    db: sqlx::SqlitePool,
    process_manager_actor: Address<process_manager::Actor>,
    order: Order,
    quantity: Usd,
    n_payouts: usize,
    oracle_pk: schnorrsig::PublicKey,
    announcement: Announcement,
    build_party_params: Box<dyn MessageChannel<wallet::BuildPartyParams>>,
    sign: Box<dyn MessageChannel<wallet::Sign>>,
    taker: Box<dyn MessageChannel<maker_inc_connections::TakerMessage>>,
    confirm_order: Box<dyn MessageChannel<maker_inc_connections::ConfirmOrder>>,
    taker_id: Identity,
    on_completed: Box<dyn MessageChannel<SetupCompleted>>,
    on_stopping: Vec<Box<dyn MessageChannel<Stopping<Self>>>>,
    setup_msg_sender: Option<UnboundedSender<SetupMsg>>,
}

impl Actor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: sqlx::SqlitePool,
        process_manager_actor: Address<process_manager::Actor>,
        (order, quantity, n_payouts): (Order, Usd, usize),
        (oracle_pk, announcement): (schnorrsig::PublicKey, Announcement),
        build_party_params: &(impl MessageChannel<wallet::BuildPartyParams> + 'static),
        sign: &(impl MessageChannel<wallet::Sign> + 'static),
        (taker, confirm_order, taker_id): (
            &(impl MessageChannel<maker_inc_connections::TakerMessage> + 'static),
            &(impl MessageChannel<maker_inc_connections::ConfirmOrder> + 'static),
            Identity,
        ),
        on_completed: &(impl MessageChannel<SetupCompleted> + 'static),
        (on_stopping0, on_stopping1): (
            &(impl MessageChannel<Stopping<Self>> + 'static),
            &(impl MessageChannel<Stopping<Self>> + 'static),
        ),
    ) -> Self {
        Self {
            db,
            process_manager_actor,
            order,
            quantity,
            n_payouts,
            oracle_pk,
            announcement,
            build_party_params: build_party_params.clone_channel(),
            sign: sign.clone_channel(),
            taker: taker.clone_channel(),
            confirm_order: confirm_order.clone_channel(),
            taker_id,
            on_completed: on_completed.clone_channel(),
            on_stopping: vec![on_stopping0.clone_channel(), on_stopping1.clone_channel()],
            setup_msg_sender: None,
        }
    }

    async fn contract_setup(&mut self, this: xtra::Address<Self>) -> Result<()> {
        let order_id = self.order.id;

        let (sender, receiver) = mpsc::unbounded();
        // store the writing end to forward messages from the taker to
        // the spawned contract setup task
        self.setup_msg_sender = Some(sender);

        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd(order_id, &mut conn).await?;
        let (event, setup_params) = cfd.start_contract_setup()?;
        apply_event(&self.process_manager_actor, event).await?;

        let taker_id = setup_params.counterparty_identity();

        let contract_future = setup_contract::new(
            self.taker.sink().with(move |msg| {
                future::ok(maker_inc_connections::TakerMessage {
                    taker_id,
                    msg: wire::MakerToTaker::Protocol { order_id, msg },
                })
            }),
            receiver,
            (self.oracle_pk, self.announcement.clone()),
            setup_params,
            self.build_party_params.clone_channel(),
            self.sign.clone_channel(),
            Role::Maker,
            self.n_payouts,
        );

        spawn_fallible::<_, anyhow::Error>(async move {
            let _ = match contract_future.await {
                Ok(dlc) => this.send(SetupSucceeded { order_id, dlc }).await?,
                Err(error) => this.send(SetupFailed { order_id, error }).await?,
            };

            Ok(())
        });

        Ok(())
    }

    async fn complete(&mut self, completed: SetupCompleted, ctx: &mut xtra::Context<Self>) {
        let _ = self
            .on_completed
            .send(completed)
            .log_failure("Failed to inform about contract setup completion")
            .await;

        ctx.stop();
    }
}

#[xtra_productivity]
impl Actor {
    fn handle(&mut self, _msg: Accepted, ctx: &mut xtra::Context<Self>) {
        let order_id = self.order.id;

        if self.setup_msg_sender.is_some() {
            tracing::warn!(%order_id, "Contract setup already active");
            return;
        }

        tracing::info!(%order_id, "Maker accepts an order");

        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");

        let fut = async {
            self.confirm_order
                .send(maker_inc_connections::ConfirmOrder {
                    taker_id: self.taker_id,
                    order_id,
                    address: this.clone(),
                })
                .await
                .context("Failed to deliver order confirmation")??;

            self.contract_setup(this)
                .await
                .context("Failed to start contract setup")?;

            Ok(())
        };

        if let Err(error) = fut.await {
            tracing::warn!(%order_id, "Stopping setup_maker actor: {}", error);

            self.complete(SetupCompleted::Failed { order_id, error }, ctx)
                .await;

            return;
        }
    }

    fn handle(&mut self, _msg: Rejected, ctx: &mut xtra::Context<Self>) {
        let _ = self
            .taker
            .send(TakerMessage {
                taker_id: self.taker_id,
                msg: MakerToTaker::RejectOrder(self.order.id),
            })
            .log_failure("Failed to reject order to taker")
            .await;

        // We cannot use completed here because we are sending a message to ourselves and using
        // `send` would be a deadlock!
        let _ = self
            .on_completed
            .send_async_safe(SetupCompleted::rejected(self.order.id))
            .await;

        ctx.stop();
    }

    fn handle(&mut self, msg: SetupSucceeded, ctx: &mut xtra::Context<Self>) {
        self.complete(SetupCompleted::succeeded(msg.order_id, msg.dlc), ctx)
            .await
    }

    fn handle(&mut self, msg: SetupFailed, ctx: &mut xtra::Context<Self>) {
        self.complete(
            SetupCompleted::Failed {
                order_id: msg.order_id,
                error: msg.error,
            },
            ctx,
        )
        .await
    }
}

#[xtra_productivity(message_impl = false)]
impl Actor {
    fn handle(&mut self, msg: wire::SetupMsg, _ctx: &mut xtra::Context<Self>) -> Result<()> {
        let mut sender = self
            .setup_msg_sender
            .clone()
            .context("Cannot forward message to contract setup task")?;
        sender.send(msg).await?;

        Ok(())
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let quantity = self.quantity;
        if quantity < self.order.min_quantity || quantity > self.order.max_quantity {
            let reason = format!(
                "Order rejected: quantity {} not in range [{}, {}]",
                quantity, self.order.min_quantity, self.order.max_quantity
            );
            tracing::info!("{}", reason.clone());

            let _ = self
                .taker
                .send(maker_inc_connections::TakerMessage {
                    taker_id: self.taker_id,
                    msg: wire::MakerToTaker::RejectOrder(self.order.id),
                })
                .await;

            self.complete(
                SetupCompleted::rejected_due_to(self.order.id, anyhow::format_err!(reason)),
                ctx,
            )
            .await;
        }
    }

    async fn stopping(&mut self, ctx: &mut xtra::Context<Self>) -> xtra::KeepRunning {
        // inform other actors that we are stopping so that our
        // address can be GCd from their AddressMaps
        let me = ctx.address().expect("we are still alive");

        for channel in self.on_stopping.iter() {
            let _ = channel.send(Stopping { me: me.clone() }).await;
        }

        xtra::KeepRunning::StopAll
    }
}

/// Message sent from the `maker_cfd::Actor` to the
/// `setup_maker::Actor` to inform that the maker user has accepted
/// the taker order request from the taker.
pub struct Accepted;

/// Message sent from the `maker_cfd::Actor` to the
/// `setup_maker::Actor` to inform that the maker user has rejected
/// the taker order request from the taker.
pub struct Rejected;

/// Message sent from the spawned task to `setup_maker::Actor` to
/// notify that the contract setup has finished successfully.
pub struct SetupSucceeded {
    order_id: OrderId,
    dlc: Dlc,
}

/// Message sent from the spawned task to `setup_maker::Actor` to
/// notify that the contract setup has failed.
pub struct SetupFailed {
    order_id: OrderId,
    error: anyhow::Error,
}
