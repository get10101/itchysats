use crate::command;
use crate::connection;
use crate::db;
use crate::oracle;
use crate::process_manager;
use crate::setup_contract;
use crate::wire;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use futures::channel::mpsc;
use futures::channel::mpsc::UnboundedSender;
use futures::future;
use futures::SinkExt;
use maia::secp256k1_zkp::schnorrsig;
use model::olivia::BitMexPriceEventId;
use model::Dlc;
use model::FeeFlow;
use model::FundingFee;
use model::FundingRate;
use model::OrderId;
use model::Role;
use model::Timestamp;
use model::TxFeeRate;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::prelude::MessageChannel;
use xtra::Disconnected;
use xtra::KeepRunning;
use xtra_productivity::xtra_productivity;
use xtras::address_map::IPromiseIamReturningStopAllFromStopping;

/// The maximum amount of time we give the maker to send us a response.
const MAKER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Actor {
    id: OrderId,
    n_payouts: usize,
    oracle_pk: schnorrsig::PublicKey,
    maker: xtra::Address<connection::Actor>,
    get_announcement: Box<dyn MessageChannel<oracle::GetAnnouncement>>,
    rollover_msg_sender: Option<UnboundedSender<wire::RolloverMsg>>,
    executor: command::Executor,
    tasks: Tasks,
}

impl Actor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: OrderId,
        n_payouts: usize,
        oracle_pk: schnorrsig::PublicKey,
        maker: xtra::Address<connection::Actor>,
        get_announcement: &(impl MessageChannel<oracle::GetAnnouncement> + 'static),
        process_manager: xtra::Address<process_manager::Actor>,
        db: db::Connection,
    ) -> Self {
        Self {
            id,
            n_payouts,
            oracle_pk,
            maker,
            get_announcement: get_announcement.clone_channel(),
            rollover_msg_sender: None,
            tasks: Tasks::default(),
            executor: command::Executor::new(db, process_manager),
        }
    }

    async fn propose(&mut self, this: xtra::Address<Self>) -> Result<()> {
        self.executor
            .execute(self.id, |cfd| cfd.start_rollover())
            .await?;

        tracing::trace!(order_id=%self.id, "Proposing rollover");

        self.maker
            .send(connection::ProposeRollover {
                order_id: self.id,
                timestamp: Timestamp::now(),
                address: this,
            })
            .await
            .context("Failed to propose rollover")??;

        Ok(())
    }

    async fn handle_confirmed(
        &mut self,
        msg: RolloverAccepted,
        ctx: &mut xtra::Context<Self>,
    ) -> Result<()> {
        let RolloverAccepted {
            oracle_event_id,
            tx_fee_rate,
            funding_rate,
            complete_fee,
        }: RolloverAccepted = msg;
        let order_id = self.id;

        let (rollover_params, dlc, position) = self
            .executor
            .execute(self.id, |cfd| {
                cfd.handle_rollover_accepted_taker(tx_fee_rate, funding_rate)
            })
            .await?;

        let announcement = self
            .get_announcement
            .send(oracle::GetAnnouncement(oracle_event_id))
            .await
            .context("Oracle actor disconnected")?
            .context("Failed to get announcement")?;

        tracing::info!(%order_id, "Rollover proposal got accepted");

        let (sender, receiver) = mpsc::unbounded::<wire::RolloverMsg>();
        // store the writing end to forward messages from the maker to
        // the spawned rollover task
        self.rollover_msg_sender = Some(sender);

        let funding_fee = *rollover_params.funding_fee();

        let rollover_fut = setup_contract::roll_over(
            xtra::message_channel::MessageChannel::sink(&self.maker).with(move |msg| {
                future::ok(wire::TakerToMaker::RolloverProtocol { order_id, msg })
            }),
            receiver,
            (self.oracle_pk, announcement),
            rollover_params,
            Role::Taker,
            position,
            dlc,
            self.n_payouts,
            complete_fee,
        );

        let this = ctx.address().expect("self to be alive");
        self.tasks.add(async move {
            // Use an explicit type annotation to cause a compile error if someone changes the
            // handler.
            let _: Result<(), Disconnected> =
                match rollover_fut.await.context("Rollover protocol failed") {
                    Ok(dlc) => this.send(RolloverSucceeded { dlc, funding_fee }).await,
                    Err(error) => this.send(RolloverFailed { error }).await,
                };
        });

        Ok(())
    }

    async fn forward_protocol_msg(&mut self, msg: wire::RolloverMsg) -> Result<()> {
        self.rollover_msg_sender
            .as_mut()
            .context("Rollover task is not active")? // Sender is set once `Accepted` is received.
            .send(msg)
            .await
            .context("Failed to forward message to rollover task")?;

        Ok(())
    }

    async fn emit_complete(
        &mut self,
        dlc: Dlc,
        funding_fee: FundingFee,
        ctx: &mut xtra::Context<Self>,
    ) {
        let result = self
            .executor
            .execute(self.id, |cfd| Ok(cfd.complete_rollover(dlc, funding_fee)))
            .await;

        if let Err(e) = result {
            tracing::warn!(order_id = %self.id, "Failed to complete rollover: {:#}", e)
        }

        ctx.stop();
    }

    async fn emit_reject(&mut self, reason: anyhow::Error, ctx: &mut xtra::Context<Self>) {
        if let Err(e) = self
            .executor
            .execute(self.id, |cfd| Ok(cfd.reject_rollover(reason)))
            .await
        {
            tracing::warn!(order_id = %self.id, "{:#}", e)
        }

        ctx.stop();
    }

    async fn emit_fail(&mut self, error: anyhow::Error, ctx: &mut xtra::Context<Self>) {
        if let Err(e) = self
            .executor
            .execute(self.id, |cfd| Ok(cfd.fail_rollover(error)))
            .await
        {
            tracing::warn!(order_id = %self.id, "{:#}", e)
        }

        ctx.stop();
    }

    /// Returns whether the maker has accepted our rollover proposal.
    fn is_accepted(&self) -> bool {
        self.rollover_msg_sender.is_some()
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("self to be alive");

        if let Err(error) = self.propose(this).await {
            self.emit_fail(error, ctx).await;

            return;
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

    async fn stopping(&mut self, _: &mut xtra::Context<Self>) -> KeepRunning {
        KeepRunning::StopAll
    }

    async fn stopped(self) -> Self::Stop {}
}

impl IPromiseIamReturningStopAllFromStopping for Actor {}

#[xtra_productivity]
impl Actor {
    pub async fn handle_confirm_rollover(
        &mut self,
        msg: RolloverAccepted,
        ctx: &mut xtra::Context<Self>,
    ) {
        if let Err(error) = self.handle_confirmed(msg, ctx).await {
            self.emit_fail(error, ctx).await;
        }
    }

    pub async fn reject_rollover(&mut self, _: RolloverRejected, ctx: &mut xtra::Context<Self>) {
        let order_id = self.id;

        tracing::info!(%order_id, "Rollover proposal got rejected");

        self.emit_reject(anyhow::format_err!("unknown"), ctx).await;
    }

    pub async fn handle_rollover_succeeded(
        &mut self,
        msg: RolloverSucceeded,
        ctx: &mut xtra::Context<Self>,
    ) {
        self.emit_complete(msg.dlc, msg.funding_fee, ctx).await;
    }

    pub async fn handle_rollover_failed(
        &mut self,
        msg: RolloverFailed,
        ctx: &mut xtra::Context<Self>,
    ) {
        self.emit_fail(msg.error, ctx).await;
    }

    pub async fn handle_rollover_timeout_reached(
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
        self.emit_fail(
            anyhow!("Maker did not respond within {timeout} seconds"),
            ctx,
        )
        .await;
    }

    pub async fn handle_protocol_msg(
        &mut self,
        msg: wire::RolloverMsg,
        ctx: &mut xtra::Context<Self>,
    ) {
        if let Err(error) = self.forward_protocol_msg(msg).await {
            self.emit_fail(error, ctx).await;
        }
    }
}

/// Message sent from the `connection::Actor` to the
/// `rollover_taker::Actor` to notify that the maker has accepted the
/// rollover proposal.
#[derive(Clone, Copy)]
pub struct RolloverAccepted {
    pub oracle_event_id: BitMexPriceEventId,
    pub tx_fee_rate: TxFeeRate,
    pub funding_rate: FundingRate,
    pub complete_fee: FeeFlow,
}

/// Message sent from the `connection::Actor` to the
/// `rollover_taker::Actor` to notify that the maker has rejected the
/// rollover proposal.
#[derive(Clone, Copy)]
pub struct RolloverRejected;

/// Message sent from the spawned task to `rollover_taker::Actor` to
/// notify that rollover has finished successfully.
struct RolloverSucceeded {
    dlc: Dlc,
    funding_fee: FundingFee,
}

/// Message sent from the spawned task to `rollover_taker::Actor` to
/// notify that rollover has failed.
struct RolloverFailed {
    error: anyhow::Error,
}

/// Message sent from the spawned task to `rollover_taker::Actor` to
/// notify that the timeout has been reached.
///
/// It is up to the actor to reason whether or not the protocol has progressed since then.
struct MakerResponseTimeoutReached {
    timeout: Duration,
}
