use crate::address_map::ActorName;
use crate::address_map::Stopping;
use crate::cfd_actors::load_cfd;
use crate::connection;
use crate::model::cfd;
use crate::model::cfd::CfdEvent;
use crate::model::cfd::CollaborativeSettlement;
use crate::model::cfd::Completed;
use crate::model::cfd::OrderId;
use crate::model::cfd::SettlementKind;
use crate::model::cfd::SettlementProposal;
use crate::model::Price;
use crate::process_manager;
use crate::projection;
use crate::send_async_safe::SendAsyncSafe;
use crate::wire;
use anyhow::Result;
use async_trait::async_trait;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    proposal: Option<SettlementProposal>,
    order_id: OrderId,
    current_price: Price,
    n_payouts: usize,
    connection: xtra::Address<connection::Actor>,
    process_manager: xtra::Address<process_manager::Actor>,
    projection: xtra::Address<projection::Actor>,
    db: sqlx::SqlitePool,
}

impl Actor {
    pub fn new(
        order_id: OrderId,
        current_price: Price,
        n_payouts: usize,
        connection: xtra::Address<connection::Actor>,
        process_manager: xtra::Address<process_manager::Actor>,
        projection: xtra::Address<projection::Actor>,
        db: sqlx::SqlitePool,
    ) -> Self {
        Self {
            proposal: None,
            order_id,
            n_payouts,
            current_price,
            connection,
            process_manager,
            projection,
            db,
        }
    }

    async fn propose(&mut self, this: xtra::Address<Self>) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd(self.order_id, &mut conn).await?;

        let event = cfd.propose_collaborative_settlement(self.current_price, self.n_payouts)?;
        let proposal = if let cfd::Event {
            event: CfdEvent::CollaborativeSettlementProposed { ref proposal },
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

        self.update_proposal(None).await?;

        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd(order_id, &mut conn).await?;

        // TODO: This should happen within a dedicated state machine returned from
        // start_collaborative_settlement
        let proposal = self.proposal.take().expect("proposal to exist");
        let (tx, sig, payout_script_pubkey) =
            cfd.sign_collaborative_close_transaction_taker(&proposal)?;

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

    async fn handle_rejected(&mut self) -> Result<()> {
        let order_id = self.order_id;

        tracing::info!(%order_id, "Settlement proposal got rejected");

        self.update_proposal(None).await?;

        Ok(())
    }

    async fn update_proposal(
        &mut self,
        proposal: Option<(SettlementProposal, SettlementKind)>,
    ) -> Result<()> {
        self.projection
            .send(projection::UpdateSettlementProposal {
                order: self.order_id,
                proposal,
            })
            .await?;

        Ok(())
    }

    async fn complete(
        &mut self,
        completed: Completed<CollaborativeSettlement>,
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
    }

    async fn stopping(&mut self, ctx: &mut xtra::Context<Self>) -> xtra::KeepRunning {
        // inform the connection actor that we stopping so it can GC the address from the hashmap
        let me = ctx.address().expect("we are still alive");
        let _ = self.connection.send(Stopping { me }).await;

        xtra::KeepRunning::StopAll
    }

    async fn stopped(mut self) {
        let _ = self.update_proposal(None).await;
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(
        &mut self,
        msg: wire::maker_to_taker::Settlement,
        ctx: &mut xtra::Context<Self>,
    ) {
        let order_id = self.order_id;

        let completed = match msg {
            wire::maker_to_taker::Settlement::Confirm => match self.handle_confirmed().await {
                Ok(settlement) => Completed::Succeeded {
                    order_id,
                    payload: settlement,
                },
                Err(e) => Completed::Failed { error: e, order_id },
            },
            wire::maker_to_taker::Settlement::Reject => {
                if let Err(e) = self.handle_rejected().await {
                    // XXX: Should this be rejected_due_to(order_id, e) instead?
                    Completed::Failed { error: e, order_id }
                } else {
                    Completed::rejected(order_id)
                }
            }
        };

        self.complete(completed, ctx).await;
    }
}

impl ActorName for Actor {
    fn actor_name() -> String {
        "Taker collab settlement".to_string()
    }
}
