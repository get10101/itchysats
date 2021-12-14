use crate::address_map::ActorName;
use crate::address_map::Stopping;
use crate::maker_inc_connections;
use crate::model::cfd::Cfd;
use crate::model::cfd::Dlc;
use crate::model::cfd::Order;
use crate::model::cfd::OrderId;
use crate::model::cfd::Role;
use crate::model::Identity;
use crate::oracle::Announcement;
use crate::setup_contract;
use crate::setup_contract::SetupParams;
use crate::tokio_ext::spawn_fallible;
use crate::wallet;
use crate::wire;
use crate::wire::SetupMsg;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use futures::channel::mpsc;
use futures::channel::mpsc::UnboundedSender;
use futures::future;
use futures::SinkExt;
use maia::secp256k1_zkp::schnorrsig;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    cfd: Cfd,
    order: Order,
    n_payouts: usize,
    oracle_pk: schnorrsig::PublicKey,
    announcement: Announcement,
    build_party_params: Box<dyn MessageChannel<wallet::BuildPartyParams>>,
    sign: Box<dyn MessageChannel<wallet::Sign>>,
    taker: Box<dyn MessageChannel<maker_inc_connections::TakerMessage>>,
    confirm_order: Box<dyn MessageChannel<maker_inc_connections::ConfirmOrder>>,
    taker_id: Identity,
    on_completed: Box<dyn MessageChannel<Completed>>,
    on_stopping: Vec<Box<dyn MessageChannel<Stopping<Self>>>>,
    setup_msg_sender: Option<UnboundedSender<SetupMsg>>,
}

impl Actor {
    pub fn new(
        (cfd, order, n_payouts): (Cfd, Order, usize),
        (oracle_pk, announcement): (schnorrsig::PublicKey, Announcement),
        build_party_params: &(impl MessageChannel<wallet::BuildPartyParams> + 'static),
        sign: &(impl MessageChannel<wallet::Sign> + 'static),
        (taker, confirm_order, taker_id): (
            &(impl MessageChannel<maker_inc_connections::TakerMessage> + 'static),
            &(impl MessageChannel<maker_inc_connections::ConfirmOrder> + 'static),
            Identity,
        ),
        on_completed: &(impl MessageChannel<Completed> + 'static),
        (on_stopping0, on_stopping1): (
            &(impl MessageChannel<Stopping<Self>> + 'static),
            &(impl MessageChannel<Stopping<Self>> + 'static),
        ),
    ) -> Self {
        Self {
            cfd,
            order,
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
        let order_id = self.cfd.id();

        let (sender, receiver) = mpsc::unbounded();
        // store the writing end to forward messages from the taker to
        // the spawned contract setup task
        self.setup_msg_sender = Some(sender);

        let taker_id = self.taker_id;
        let contract_future = setup_contract::new(
            self.taker.sink().with(move |msg| {
                future::ok(maker_inc_connections::TakerMessage {
                    taker_id,
                    msg: wire::MakerToTaker::Protocol { order_id, msg },
                })
            }),
            receiver,
            (self.oracle_pk, self.announcement.clone()),
            SetupParams::new(
                self.cfd.margin()?,
                self.cfd.counterparty_margin()?,
                self.cfd.price(),
                self.cfd.quantity_usd(),
                self.cfd.leverage(),
                self.cfd.refund_timelock_in_blocks(),
                self.cfd.fee_rate(),
            ),
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

    async fn complete(&mut self, completed: Completed, ctx: &mut xtra::Context<Self>) {
        let _ = self.on_completed.send(completed).await;

        ctx.stop();
    }
}

#[xtra_productivity]
impl Actor {
    fn handle(&mut self, _msg: Accepted, ctx: &mut xtra::Context<Self>) {
        let order_id = self.cfd.id();
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

            self.complete(Completed::Failed { order_id, error }, ctx)
                .await;

            return;
        }
    }

    fn handle(&mut self, _msg: Rejected, ctx: &mut xtra::Context<Self>) {
        self.complete(Completed::Rejected(self.cfd.id()), ctx).await;
    }

    fn handle(&mut self, msg: SetupSucceeded, ctx: &mut xtra::Context<Self>) {
        self.complete(
            Completed::NewContract {
                order_id: msg.order_id,
                dlc: msg.dlc,
            },
            ctx,
        )
        .await
    }

    fn handle(&mut self, msg: SetupFailed, ctx: &mut xtra::Context<Self>) {
        self.complete(
            Completed::Failed {
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
        let quantity = self.cfd.quantity_usd();
        let cfd = self.cfd.clone();
        if quantity < self.order.min_quantity || quantity > self.order.max_quantity {
            tracing::info!(
                "Order rejected: quantity {} not in range [{}, {}]",
                quantity,
                self.order.min_quantity,
                self.order.max_quantity
            );

            let _ = self
                .taker
                .send(maker_inc_connections::TakerMessage {
                    taker_id: self.taker_id,
                    msg: wire::MakerToTaker::RejectOrder(cfd.id()),
                })
                .await;

            self.complete(Completed::Rejected(cfd.id()), ctx).await;
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

/// Message sent from the `setup_maker::Actor` to the
/// `maker_cfd::Actor` to notify that the contract setup has started.
pub struct Started(pub OrderId);

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

/// Message sent from the `setup_maker::Actor` to the
/// `maker_cfd::Actor` to notify that the contract setup has finished.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Completed {
    Rejected(OrderId),
    NewContract {
        order_id: OrderId,
        dlc: Dlc,
    },
    Failed {
        order_id: OrderId,
        error: anyhow::Error,
    },
}

impl xtra::Message for Started {
    type Result = ();
}

impl ActorName for Actor {
    fn actor_name() -> String {
        "Maker contract setup".to_string()
    }
}
