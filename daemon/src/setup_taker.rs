use crate::bitcoin::util::psbt::PartiallySignedTransaction;
use crate::command;
use crate::connection;
use crate::process_manager;
use crate::setup_contract_deprecated;
use crate::wallet;
use crate::wire;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use futures::channel::mpsc;
use futures::channel::mpsc::UnboundedSender;
use futures::future;
use futures::SinkExt;
use maia_core::secp256k1_zkp::schnorrsig;
use maia_core::PartyParams;
use model::olivia::Announcement;
use model::Dlc;
use model::Leverage;
use model::OrderId;
use model::Role;
use model::Usd;
use sqlite_db;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::message_channel::MessageChannel;
use xtra_productivity::xtra_productivity;

/// The maximum amount of time we give the maker to send us a response.
const MAKER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Actor {
    order_id: OrderId,
    quantity: Usd,
    leverage: Leverage,
    n_payouts: usize,
    oracle_pk: schnorrsig::PublicKey,
    announcement: Announcement,
    build_party_params: MessageChannel<wallet::BuildPartyParams, Result<PartyParams>>,
    sign: MessageChannel<wallet::Sign, Result<PartiallySignedTransaction>>,
    maker: xtra::Address<connection::Actor>,
    setup_msg_sender: Option<UnboundedSender<wire::SetupMsg>>,
    tasks: Tasks,
    executor: command::Executor,
}

impl Actor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: sqlite_db::Connection,
        process_manager: xtra::Address<process_manager::Actor>,
        (order_id, quantity, leverage, n_payouts): (OrderId, Usd, Leverage, usize),
        (oracle_pk, announcement): (schnorrsig::PublicKey, Announcement),
        build_party_params: MessageChannel<wallet::BuildPartyParams, Result<PartyParams>>,
        sign: MessageChannel<wallet::Sign, Result<PartiallySignedTransaction>>,
        maker: xtra::Address<connection::Actor>,
    ) -> Self {
        Self {
            order_id,
            quantity,
            leverage,
            n_payouts,
            oracle_pk,
            announcement,
            build_party_params,
            sign,
            maker,
            setup_msg_sender: None,
            tasks: Tasks::default(),
            executor: command::Executor::new(db, process_manager),
        }
    }

    /// Returns whether the maker has accepted our setup proposal.
    fn is_accepted(&self) -> bool {
        self.setup_msg_sender.is_some()
    }

    async fn forward_protocol_msg(&self, msg: wire::SetupMsg) -> Result<()> {
        let mut sender = self
            .setup_msg_sender
            .clone()
            .context("Cannot forward message to contract setup task")?;
        sender.send(msg).await?;

        Ok(())
    }
}

#[xtra_productivity]
impl Actor {
    fn handle(&mut self, _: Accepted, ctx: &mut xtra::Context<Self>) {
        let order_id = self.order_id;
        tracing::info!(%order_id, "Order got accepted");

        let (setup_params, position) = match self
            .executor
            .execute(order_id, |cfd| cfd.start_contract_setup())
            .await
        {
            Ok(contract_setup) => contract_setup,
            Err(e) => {
                tracing::error!("Failed to handle accepting contract setup: {e}");
                return;
            }
        };

        let (sender, receiver) = mpsc::unbounded();
        // store the writing end to forward messages from the maker to
        // the spawned contract setup task
        self.setup_msg_sender = Some(sender);

        let contract_future = setup_contract_deprecated::new(
            self.maker
                .clone()
                .into_sink()
                .with(move |msg| future::ok(wire::TakerToMaker::Protocol { order_id, msg })),
            receiver,
            (self.oracle_pk, self.announcement.clone()),
            setup_params,
            self.build_party_params.clone(),
            self.sign.clone(),
            Role::Taker,
            position,
            self.n_payouts,
        );

        let this = ctx.address().expect("self to be alive");
        self.tasks.add(async move {
            let _: Result<(), xtra::Error> = match contract_future.await {
                Ok(dlc) => this.send(SetupSucceeded { dlc }).await,
                Err(error) => this.send(SetupFailed { error }).await,
            };
        });
    }

    fn handle(&mut self, msg: Rejected, ctx: &mut xtra::Context<Self>) {
        let order_id = self.order_id;
        tracing::info!(%order_id, "Order got rejected");

        let reason = if msg.is_invalid_order {
            anyhow::format_err!("Invalid order id: {order_id}")
        } else {
            anyhow::format_err!("Unknown")
        };

        if let Err(e) = self
            .executor
            .execute(order_id, |cfd| cfd.reject_contract_setup(reason))
            .await
        {
            tracing::warn!("Failed to execute `reject_contract_setup` command: {e:#}");
        }

        ctx.stop_self();
    }

    fn handle(&mut self, msg: wire::SetupMsg) {
        if let Err(e) = self.forward_protocol_msg(msg).await {
            tracing::error!("Failed to forward protocol message: {e:#}")
        }
    }

    fn handle(&mut self, msg: SetupSucceeded, ctx: &mut xtra::Context<Self>) {
        if let Err(e) = self
            .executor
            .execute(self.order_id, |cfd| cfd.complete_contract_setup(msg.dlc))
            .await
        {
            tracing::warn!("Failed to execute `complete_contract_setup` command: {e:#}");
        }

        ctx.stop_self();
    }

    fn handle(&mut self, msg: SetupFailed, ctx: &mut xtra::Context<Self>) {
        if let Err(e) = self
            .executor
            .execute(self.order_id, |cfd| Ok(cfd.fail_contract_setup(msg.error)))
            .await
        {
            tracing::warn!("Failed to execute `fail_contract_setup` command: {e:#}");
        }

        ctx.stop_self();
    }

    pub async fn handle_setup_timeout_reached(
        &mut self,
        msg: MakerResponseTimeoutReached,
        ctx: &mut xtra::Context<Self>,
    ) {
        // If we are accepted, discard the timeout because the maker DID respond.
        if self.is_accepted() {
            return;
        }

        // Otherwise, fail because we did not receive a response.
        // If the proposal is rejected, our entire actor would already be shut down and we hence
        // never get this message.
        let timeout = msg.timeout.as_secs();
        if let Err(e) = self
            .executor
            .execute(self.order_id, |cfd| {
                Ok(cfd
                    .fail_contract_setup(anyhow!("Maker did not respond within {timeout} seconds")))
            })
            .await
        {
            tracing::warn!("Failed to execute `fail_contract_setup` command: {:#}", e);
        }

        ctx.stop_self();
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let address = ctx
            .address()
            .expect("actor to be able to give address to itself");

        let res = self
            .maker
            .send(connection::TakeOrder {
                order_id: self.order_id,
                quantity: self.quantity,
                leverage: self.leverage,
                address,
            })
            .await;

        if let Err(e) = res {
            tracing::warn!(id = %self.order_id, "Stopping setup_taker actor: {e}");
            ctx.stop_self();
        }

        let maker_response_timeout = {
            let this = ctx.address().expect("self to be alive");
            async move {
                tokio::time::sleep(MAKER_RESPONSE_TIMEOUT).await;

                let _ = this
                    .send(MakerResponseTimeoutReached {
                        timeout: MAKER_RESPONSE_TIMEOUT,
                    })
                    .await;
            }
        };

        self.tasks.add(maker_response_timeout);
    }

    async fn stopped(self) -> Self::Stop {}
}

/// Message sent from the `connection::Actor` to the
/// `setup_taker::Actor` to notify that the order taken was accepted
/// by the maker.
#[derive(Clone, Copy)]
pub struct Accepted;

/// Message sent from the `connection::Actor` to the
/// `setup_taker::Actor` to notify that the order taken was rejected
/// by the maker.
#[derive(Clone, Copy)]
pub struct Rejected {
    /// Used to indicate whether the rejection stems from the order ID
    /// not being recognised by the maker.
    is_invalid_order: bool,
}

/// Message sent from the spawned task to `setup_taker::Actor` to
/// notify that the contract setup has finished successfully.
struct SetupSucceeded {
    dlc: Dlc,
}

/// Message sent from the spawned task to `setup_taker::Actor` to
/// notify that the contract setup has failed.
struct SetupFailed {
    error: anyhow::Error,
}

impl Rejected {
    /// Order was rejected by the maker for not specific reason.
    pub fn without_reason() -> Self {
        Rejected {
            is_invalid_order: false,
        }
    }

    /// Order was rejected by the maker because it did not recognise
    /// the order ID provided.
    pub fn invalid_order_id() -> Self {
        Rejected {
            is_invalid_order: true,
        }
    }
}

/// Message sent from the spawned task to `setup_taker::Actor` to
/// notify that the timeout has been reached.
///
/// It is up to the actor to reason whether or not the protocol has progressed since then.
struct MakerResponseTimeoutReached {
    timeout: Duration,
}
