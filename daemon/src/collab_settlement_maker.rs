use crate::command;

use crate::maker_inc_connections;
use crate::process_manager;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use maia::secp256k1_zkp::Signature;
use model::CollaborativeSettlement;
use model::Identity;
use model::SettlementProposal;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::prelude::MessageChannel;
use xtra::KeepRunning;
use xtra_productivity::xtra_productivity;
use xtras::address_map::IPromiseIamReturningStopAllFromStopping;

/// Timeout for waiting for the `Initiate` message from the taker
///
/// This timeout is started when handling accept.
/// If the taker does not come back with `Initiate` until the timeout is triggered we fail the
/// settlement. If the taker does come back with `Initiate` before the timeout is reached, we don't
/// fail the settlement even if the timeout is triggered.
const INITIATE_TIMEOUT: Duration = Duration::from_secs(60 * 5);

pub struct Actor {
    proposal: SettlementProposal,
    taker_id: Identity,
    connections: Box<dyn MessageChannel<maker_inc_connections::settlement::Response>>,
    has_accepted: bool,
    is_initiated: bool,
    n_payouts: usize,
    executor: command::Executor,
    db: sqlite_db::Connection,
    tasks: Tasks,
}

#[derive(Clone, Copy)]
pub struct Accepted;

#[derive(Clone, Copy)]
pub struct Rejected;

#[derive(Clone, Copy)]
pub struct Initiated {
    pub sig_taker: Signature,
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: Accepted, ctx: &mut xtra::Context<Self>) {
        if let Err(e) = self.accept(ctx).await {
            self.emit_failed(e, ctx).await;
        }
    }

    async fn handle(&mut self, _: Rejected, ctx: &mut xtra::Context<Self>) {
        self.reject(ctx).await
    }

    async fn handle(&mut self, msg: Initiated, ctx: &mut xtra::Context<Self>) {
        self.is_initiated = true;

        match async {
            tracing::info!(
                order_id = %self.proposal.order_id,
                taker_id = %self.taker_id,
                "Received signature for collaborative settlement"
            );

            let cfd = self
                .db
                .load_open_cfd::<model::Cfd>(self.proposal.order_id, ())
                .await?;

            let settlement =
                cfd.sign_collaborative_settlement_maker(self.proposal, msg.sig_taker)?;

            anyhow::Ok(settlement)
        }
        .await
        {
            Ok(settlement) => self.emit_completed(settlement, ctx).await,
            Err(e) => self.emit_failed(e, ctx).await,
        };
    }

    pub async fn handle_initiate_timeout_reached(
        &mut self,
        msg: InitiateTimeoutReached,
        ctx: &mut xtra::Context<Self>,
    ) {
        // If the taker sent us the Initiate message we know that we will finish either by
        // completing or by failing the protocol
        if self.is_initiated {
            return;
        }

        // Otherwise, fail because the taker did not Initiate the protocol
        let timeout = msg.timeout.as_secs();
        if let Err(e) = self
            .executor
            .execute(self.proposal.order_id, |cfd| {
                Ok(cfd.fail_collaborative_settlement(anyhow!(
                    "Taker did not initiate within {timeout} seconds"
                )))
            })
            .await
        {
            tracing::warn!(
                order_id=%self.proposal.order_id, "Failed to execute `fail_collaborative_settlement` command: {:#}",
                e
            );
        }

        ctx.stop()
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let order_id = self.proposal.order_id;

        tracing::info!(
            taker_id = %self.taker_id,
            %order_id,
            price = %self.proposal.price,
            "Received settlement proposal"
        );

        if let Err(error) = self.handle_proposal().await {
            self.emit_failed(error, ctx).await;
        }
    }

    async fn stopping(&mut self, _: &mut xtra::Context<Self>) -> KeepRunning {
        KeepRunning::StopAll
    }

    async fn stopped(self) -> Self::Stop {}
}

impl IPromiseIamReturningStopAllFromStopping for Actor {}

impl Actor {
    pub fn new(
        proposal: SettlementProposal,
        taker_id: Identity,
        connections: &(impl MessageChannel<maker_inc_connections::settlement::Response> + 'static),
        process_manager: xtra::Address<process_manager::Actor>,
        db: sqlite_db::Connection,
        n_payouts: usize,
    ) -> Self {
        Self {
            proposal,
            taker_id,
            connections: connections.clone_channel(),
            has_accepted: false,
            n_payouts,
            executor: command::Executor::new(db.clone(), process_manager),
            db,
            tasks: Tasks::default(),
            is_initiated: false,
        }
    }

    async fn handle_proposal(&mut self) -> Result<()> {
        self.executor
            .execute(self.proposal.order_id, |cfd| {
                cfd.receive_collaborative_settlement_proposal(self.proposal, self.n_payouts)
            })
            .await?;

        Ok(())
    }

    async fn emit_completed(
        &mut self,
        settlement: CollaborativeSettlement,
        ctx: &mut xtra::Context<Self>,
    ) {
        let order_id = self.proposal.order_id;
        if let Err(e) = self
            .executor
            .execute(order_id, |cfd| {
                Ok(cfd.complete_collaborative_settlement(settlement))
            })
            .await
        {
            tracing::warn!(%order_id, "Failed to execute `complete_collaborative_settlement` command: {e:#}");
        }

        ctx.stop();
    }

    async fn emit_rejected(&mut self, reason: anyhow::Error, ctx: &mut xtra::Context<Self>) {
        let order_id = self.proposal.order_id;
        if let Err(e) = self
            .executor
            .execute(order_id, |cfd| {
                Ok(cfd.reject_collaborative_settlement(reason))
            })
            .await
        {
            tracing::warn!(%order_id, "Failed to execute `reject_collaborative_settlement` command: {e:#}");
        }

        ctx.stop();
    }

    async fn emit_failed(&mut self, error: anyhow::Error, ctx: &mut xtra::Context<Self>) {
        let order_id = self.proposal.order_id;
        if let Err(e) = self
            .executor
            .execute(order_id, |cfd| Ok(cfd.fail_collaborative_settlement(error)))
            .await
        {
            tracing::warn!(%order_id, "Failed to execute `fail_collaborative_settlement` command: {e:#}");
        }

        ctx.stop();
    }

    async fn accept(&mut self, ctx: &mut xtra::Context<Self>) -> Result<()> {
        let order_id = self.proposal.order_id;

        if self.has_accepted {
            tracing::warn!(%order_id, "Settlement already accepted");
            return Ok(());
        }
        self.has_accepted = true;

        tracing::info!(%order_id, "Settlement proposal accepted");

        self.executor
            .execute(order_id, |cfd| {
                cfd.accept_collaborative_settlement_proposal(&self.proposal)
            })
            .await?;

        let timeout = {
            let this = ctx.address().expect("self to be alive");
            async move {
                tokio::time::sleep(INITIATE_TIMEOUT).await;

                let _ = this
                    .send(InitiateTimeoutReached {
                        timeout: INITIATE_TIMEOUT,
                    })
                    .await;
            }
        };
        self.tasks.add(timeout);

        let this = ctx.address().expect("self to be alive");
        self.connections
            .send(maker_inc_connections::settlement::Response {
                taker_id: self.taker_id,
                order_id,
                decision: maker_inc_connections::settlement::Decision::Accept { address: this },
            })
            .await
            .context("Failed to inform taker about settlement acceptance")??;

        Ok(())
    }

    async fn reject(&mut self, ctx: &mut xtra::Context<Self>) {
        let order_id = self.proposal.order_id;
        tracing::info!(%order_id, "Settlement proposal rejected");

        let _ = self
            .connections
            .send(maker_inc_connections::settlement::Response {
                taker_id: self.taker_id,
                order_id,
                decision: maker_inc_connections::settlement::Decision::Reject,
            })
            .await;

        self.emit_rejected(anyhow::format_err!("unknown"), ctx)
            .await;
    }
}

/// Message sent from the spawned task to `collab_settlement_maker::Actor` to
/// notify that the timeout has been reached.
///
/// It is up to the actor to reason whether or not the protocol has progressed since then.
struct InitiateTimeoutReached {
    timeout: Duration,
}
