use crate::command;
use crate::maker_inc_connections;
use crate::maker_inc_connections::TakerMessage;
use crate::process_manager;
use crate::setup_contract;
use crate::wallet;
use crate::wire;
use crate::wire::MakerToTaker;
use crate::wire::SetupMsg;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use futures::channel::mpsc;
use futures::channel::mpsc::UnboundedSender;
use futures::future;
use futures::SinkExt;
use maia::secp256k1_zkp::schnorrsig;
use model::cfd::Dlc;
use model::cfd::Order;
use model::cfd::OrderId;
use model::cfd::Role;
use model::cfd::SetupCompleted;
use model::olivia::Announcement;
use model::Identity;
use model::Usd;
use tokio_tasks::Tasks;
use xtra::prelude::MessageChannel;
use xtra::Address;
use xtra_productivity::xtra_productivity;
use xtras::address_map::Stopping;
use xtras::LogFailure;

pub struct Actor {
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
    on_stopping: Vec<Box<dyn MessageChannel<Stopping<Self>>>>,
    setup_msg_sender: Option<UnboundedSender<SetupMsg>>,
    tasks: Tasks,
    executor: command::Executor,
}

impl Actor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: sqlx::SqlitePool,
        process_manager: Address<process_manager::Actor>,
        (order, quantity, n_payouts): (Order, Usd, usize),
        (oracle_pk, announcement): (schnorrsig::PublicKey, Announcement),
        build_party_params: &(impl MessageChannel<wallet::BuildPartyParams> + 'static),
        sign: &(impl MessageChannel<wallet::Sign> + 'static),
        (taker, confirm_order, taker_id): (
            &(impl MessageChannel<maker_inc_connections::TakerMessage> + 'static),
            &(impl MessageChannel<maker_inc_connections::ConfirmOrder> + 'static),
            Identity,
        ),
        (on_stopping0, on_stopping1): (
            &(impl MessageChannel<Stopping<Self>> + 'static),
            &(impl MessageChannel<Stopping<Self>> + 'static),
        ),
    ) -> Self {
        Self {
            executor: command::Executor::new(db, process_manager),
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
            on_stopping: vec![on_stopping0.clone_channel(), on_stopping1.clone_channel()],
            setup_msg_sender: None,
            tasks: Tasks::default(),
        }
    }

    async fn contract_setup(&mut self, this: xtra::Address<Self>) -> Result<()> {
        let order_id = self.order.id;

        let (sender, receiver) = mpsc::unbounded();
        // store the writing end to forward messages from the taker to
        // the spawned contract setup task
        self.setup_msg_sender = Some(sender);

        let setup_params = self
            .executor
            .execute(order_id, |cfd| cfd.start_contract_setup())
            .await?;

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

        self.tasks.add(async move {
            let _: Result<(), xtra::Disconnected> = match contract_future.await {
                Ok(dlc) => this.send(SetupSucceeded { order_id, dlc }).await,
                Err(error) => this.send(SetupFailed { order_id, error }).await,
            };
        });

        Ok(())
    }

    async fn complete(&mut self, completed: SetupCompleted, ctx: &mut xtra::Context<Self>) {
        match self
            .executor
            .execute(completed.order_id(), |cfd| cfd.setup_contract(completed))
            .await
        {
            Ok(()) => {}
            Err(e) => {
                tracing::error!("Failed to execute `contract_setup` command: {:#}", e);
            }
        }

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
            tracing::warn!(%order_id, "Stopping setup_maker actor: {error}");

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

        self.complete(SetupCompleted::rejected(self.order.id), ctx)
            .await
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
    fn handle(&mut self, msg: wire::SetupMsg) -> Result<()> {
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
    type Stop = ();
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let quantity = self.quantity;
        if quantity < self.order.min_quantity || quantity > self.order.max_quantity {
            let min = self.order.min_quantity;
            let max = self.order.max_quantity;

            let reason =
                format!("Order rejected: quantity {quantity} not in range [{min}, {max}]",);
            tracing::info!("{reason}");

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

    async fn stopped(self) -> Self::Stop {}
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
struct SetupSucceeded {
    order_id: OrderId,
    dlc: Dlc,
}

/// Message sent from the spawned task to `setup_maker::Actor` to
/// notify that the contract setup has failed.
struct SetupFailed {
    order_id: OrderId,
    error: anyhow::Error,
}
