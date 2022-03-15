use crate::cfd_actors::load_cfd;
use crate::command;
use crate::maker_inc_connections;
use crate::process_manager;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use maia::secp256k1_zkp::Signature;
use model::CollaborativeSettlement;
use model::Identity;
use model::SettlementProposal;
use xtra::prelude::MessageChannel;
use xtra::KeepRunning;
use xtra_productivity::xtra_productivity;
use xtras::address_map::IPromiseIamReturningStopAllFromStopping;

pub struct Actor {
    proposal: SettlementProposal,
    taker_id: Identity,
    connections: Box<dyn MessageChannel<maker_inc_connections::settlement::Response>>,
    has_accepted: bool,
    n_payouts: usize,
    executor: command::Executor,
    db: sqlx::PgPool,
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
        match async {
            tracing::info!(
                order_id = %self.proposal.order_id,
                taker_id = %self.taker_id,
                "Received signature for collaborative settlement"
            );

            let mut conn = self.db.acquire().await?;
            let cfd = load_cfd(self.proposal.order_id, &mut conn).await?;

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
        db: sqlx::PgPool,
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
