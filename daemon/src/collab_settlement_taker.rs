use crate::command;
use crate::connection;
use crate::db;
use crate::process_manager;
use crate::wire;
use anyhow::anyhow;
use anyhow::Result;
use async_trait::async_trait;
use model::CollaborativeSettlement;
use model::OrderId;
use model::Price;
use model::SettlementProposal;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::KeepRunning;
use xtra_productivity::xtra_productivity;
use xtras::address_map::IPromiseIamReturningStopAllFromStopping;
use xtras::SendAsyncSafe;

/// The maximum amount of time we give the maker to send us a response.
const MAKER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Actor {
    proposal: Option<SettlementProposal>,
    order_id: OrderId,
    current_price: Price,
    n_payouts: usize,
    connection: xtra::Address<connection::Actor>,
    executor: command::Executor,
    db: db::Connection,
    tasks: Tasks,
    maker_replied: bool,
}

impl Actor {
    pub fn new(
        order_id: OrderId,
        current_price: Price,
        n_payouts: usize,
        connection: xtra::Address<connection::Actor>,
        process_manager: xtra::Address<process_manager::Actor>,
        db: db::Connection,
    ) -> Self {
        Self {
            proposal: None,
            order_id,
            n_payouts,
            current_price,
            connection,
            executor: command::Executor::new(db.clone(), process_manager),
            db,
            tasks: Tasks::default(),
            maker_replied: false,
        }
    }

    async fn propose(&mut self, this: xtra::Address<Self>) -> Result<()> {
        let (proposal, ..) = self
            .executor
            .execute(self.order_id, |cfd| {
                cfd.propose_collaborative_settlement(self.current_price, self.n_payouts)
            })
            .await?;

        self.proposal = Some(proposal);

        self.connection
            .send(connection::ProposeSettlement {
                timestamp: proposal.timestamp,
                taker: proposal.taker,
                maker: proposal.maker,
                price: proposal.price,
                address: this,
                order_id: self.order_id,
            })
            .await??;

        Ok(())
    }

    async fn handle_confirmed(&mut self) -> Result<CollaborativeSettlement> {
        let order_id = self.order_id;

        tracing::info!(%order_id, "Settlement proposal got accepted");

        let cfd = self.db.load_open_cfd::<model::Cfd>(order_id, ()).await?;

        // TODO: This should happen within a dedicated state machine returned from
        // start_collaborative_settlement
        let proposal = self.proposal.take().expect("proposal to exist");
        let (tx, sig, payout_script_pubkey) = cfd.sign_collaborative_settlement_taker(&proposal)?;

        self.connection
            .send_async_safe(wire::TakerToMaker::Settlement {
                order_id,
                msg: wire::taker_to_maker::Settlement::Initiate { sig_taker: sig },
            })
            .await?;

        CollaborativeSettlement::new(tx, payout_script_pubkey, self.current_price)
    }

    async fn emit_completed(
        &mut self,
        settlement: CollaborativeSettlement,
        ctx: &mut xtra::Context<Self>,
    ) {
        let order_id = self.order_id;
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
        let order_id = self.order_id;
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
        let order_id = self.order_id;
        if let Err(e) = self
            .executor
            .execute(order_id, |cfd| Ok(cfd.fail_collaborative_settlement(error)))
            .await
        {
            tracing::warn!(%order_id, "Failed to execute `fail_collaborative_settlement` command: {e:#}");
        }

        ctx.stop();
    }

    /// Returns whether the maker has accepted our collab settlement proposal.
    fn is_accepted(&self) -> bool {
        self.maker_replied
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("get address to ourselves");

        if let Err(e) = self.propose(this).await {
            self.emit_failed(e, ctx).await;
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
    async fn handle(
        &mut self,
        msg: wire::maker_to_taker::Settlement,
        ctx: &mut xtra::Context<Self>,
    ) {
        let order_id = self.order_id;
        self.maker_replied = true;

        match msg {
            wire::maker_to_taker::Settlement::Confirm => match self.handle_confirmed().await {
                Ok(settlement) => self.emit_completed(settlement, ctx).await,
                Err(e) => self.emit_failed(e, ctx).await,
            },
            wire::maker_to_taker::Settlement::Reject => {
                tracing::info!(%order_id, "Settlement proposal got rejected");
                self.emit_rejected(anyhow::format_err!("unknown"), ctx)
                    .await
            }
        };
    }

    pub async fn handle_collab_settlement_timeout_reached(
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
                Ok(cfd.fail_collaborative_settlement(anyhow!(
                    "Maker did not respond within {timeout} seconds"
                )))
            })
            .await
        {
            tracing::warn!(
                order_id=%self.order_id, "Failed to execute `fail_collaborative_settlement` command: {:#}",
                e
            );
        }

        ctx.stop()
    }
}

/// Message sent from the spawned task to `collab_settlement_taker::Actor` to
/// notify that the timeout has been reached.
///
/// It is up to the actor to reason whether or not the protocol has progressed since then.
struct MakerResponseTimeoutReached {
    timeout: Duration,
}
