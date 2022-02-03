use crate::connection;
use anyhow::anyhow;
use anyhow::Result;
use async_trait::async_trait;
use daemon::cfd_actors::load_cfd;
use daemon::model::cfd;
use daemon::model::cfd::CfdEvent;
use daemon::model::cfd::CollaborativeSettlement;
use daemon::model::cfd::CollaborativeSettlementCompleted;
use daemon::model::cfd::Completed;
use daemon::model::cfd::OrderId;
use daemon::model::cfd::SettlementProposal;
use daemon::model::Price;
use daemon::process_manager;
use daemon::sqlx;
use daemon::wire;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra_productivity::xtra_productivity;
use xtras::address_map::Stopping;
use xtras::SendAsyncSafe;

/// The maximum amount of time we give the maker to send us a response.
const MAKER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Actor {
    proposal: Option<SettlementProposal>,
    order_id: OrderId,
    current_price: Price,
    n_payouts: usize,
    connection: xtra::Address<connection::Actor>,
    process_manager: xtra::Address<process_manager::Actor>,
    db: sqlx::SqlitePool,
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
        db: sqlx::SqlitePool,
    ) -> Self {
        Self {
            proposal: None,
            order_id,
            n_payouts,
            current_price,
            connection,
            process_manager,
            db,
            tasks: Tasks::default(),
            maker_replied: false,
        }
    }

    async fn propose(&mut self, this: xtra::Address<Self>) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd(self.order_id, &mut conn).await?;

        let event = cfd.propose_collaborative_settlement(self.current_price, self.n_payouts)?;
        let proposal = if let cfd::Event {
            event: CfdEvent::CollaborativeSettlementStarted { ref proposal },
            ..
        } = event
        {
            proposal
        } else {
            unreachable!()
        };

        self.proposal = Some(proposal.clone());

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

        self.process_manager
            .send(process_manager::Event::new(event))
            .await??;

        Ok(())
    }

    async fn handle_confirmed(&mut self) -> Result<CollaborativeSettlement> {
        let order_id = self.order_id;

        tracing::info!(%order_id, "Settlement proposal got accepted");

        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd(order_id, &mut conn).await?;

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

        Ok(CollaborativeSettlement::new(
            tx,
            payout_script_pubkey,
            self.current_price,
        )?)
    }

    async fn complete(
        &mut self,
        completed: CollaborativeSettlementCompleted,
        ctx: &mut xtra::Context<Self>,
    ) {
        let order_id = self.order_id;
        let event_fut = async {
            let mut conn = self.db.acquire().await?;
            let cfd = load_cfd(order_id, &mut conn).await?;
            let event = cfd.settle_collaboratively(completed)?;

            anyhow::Ok(event)
        };

        match event_fut.await {
            Ok(event) => {
                let _ = self
                    .process_manager
                    .send(process_manager::Event::new(event))
                    .await;
            }
            Err(e) => {
                tracing::warn!(%order_id, "Failed to report completion of collab settlement: {:#}", e)
            }
        };

        ctx.stop();
    }

    /// Returns whether the maker has accepted our collab settlement proposal.
    fn is_accepted(&self) -> bool {
        self.maker_replied
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("get address to ourselves");

        if let Err(e) = self.propose(this).await {
            self.complete(
                Completed::Failed {
                    order_id: self.order_id,
                    error: e,
                },
                ctx,
            )
            .await;
        }

        let maker_response_timeout = {
            let this = ctx.address().expect("self to be alive");
            async move {
                tokio::time::sleep(MAKER_RESPONSE_TIMEOUT).await;

                this.send(MakerResponseTimeoutReached {
                    timeout: MAKER_RESPONSE_TIMEOUT,
                })
                .await
                .expect("can send to ourselves");
            }
        };

        self.tasks.add(maker_response_timeout);
    }

    async fn stopping(&mut self, ctx: &mut xtra::Context<Self>) -> xtra::KeepRunning {
        // inform the connection actor that we stopping so it can GC the address from the hashmap
        let me = ctx.address().expect("we are still alive");
        let _ = self.connection.send(Stopping { me }).await;

        xtra::KeepRunning::StopAll
    }
}

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle(
        &mut self,
        msg: wire::maker_to_taker::Settlement,
        ctx: &mut xtra::Context<Self>,
    ) {
        let order_id = self.order_id;
        self.maker_replied = true;

        let completed = match msg {
            wire::maker_to_taker::Settlement::Confirm => match self.handle_confirmed().await {
                Ok(settlement) => Completed::Succeeded {
                    order_id,
                    payload: settlement,
                },
                Err(e) => Completed::Failed { error: e, order_id },
            },
            wire::maker_to_taker::Settlement::Reject => {
                tracing::info!(%order_id, "Settlement proposal got rejected");
                Completed::rejected(order_id)
            }
        };

        self.complete(completed, ctx).await;
    }
}

#[xtra_productivity]
impl Actor {
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
        let completed = CollaborativeSettlementCompleted::Failed {
            order_id: self.order_id,
            error: anyhow!("Maker did not respond within {timeout} seconds"),
        };

        self.complete(completed, ctx).await;
    }
}

/// Message sent from the spawned task to `collab_settlement_taker::Actor` to
/// notify that the timeout has been reached.
///
/// It is up to the actor to reason whether or not the protocol has progressed since then.
struct MakerResponseTimeoutReached {
    timeout: Duration,
}
