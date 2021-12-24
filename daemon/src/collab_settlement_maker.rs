use crate::address_map::ActorName;
use crate::address_map::Stopping;
use crate::cfd_actors::load_cfd;
use crate::maker_inc_connections;
use crate::model::cfd::CollaborativeSettlementCompleted;
use crate::model::cfd::Completed;
use crate::model::cfd::SettlementProposal;
use crate::model::Identity;
use crate::process_manager;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use maia::secp256k1_zkp::Signature;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    proposal: SettlementProposal,
    taker_id: Identity,
    connections: Box<dyn MessageChannel<maker_inc_connections::settlement::Response>>,
    process_manager: xtra::Address<process_manager::Actor>,
    on_stopping: Vec<Box<dyn MessageChannel<Stopping<Self>>>>,
    db: sqlx::SqlitePool,
}

pub struct Accepted;
pub struct Rejected;
pub struct Initiated {
    pub sig_taker: Signature,
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: Accepted, ctx: &mut xtra::Context<Self>) {
        if let Err(e) = self.accept(ctx).await {
            self.complete(
                Completed::Failed {
                    order_id: self.proposal.order_id,
                    error: e,
                },
                ctx,
            )
            .await;
        }
    }

    async fn handle(&mut self, _: Rejected, ctx: &mut xtra::Context<Self>) {
        self.reject(ctx).await
    }

    async fn handle(&mut self, msg: Initiated, ctx: &mut xtra::Context<Self>) {
        let completed = async {
            tracing::info!(
                order_id = %self.proposal.order_id,
                taker_id = %self.taker_id,
                "Received signature for collaborative settlement"
            );

            let mut conn = self.db.acquire().await?;
            let cfd = load_cfd(self.proposal.order_id, &mut conn).await?;

            let settlement =
                cfd.sign_collaborative_settlement_maker(self.proposal.clone(), msg.sig_taker)?;

            anyhow::Ok(Completed::Succeeded {
                order_id: self.proposal.order_id,
                payload: settlement,
            })
        }
        .await
        .unwrap_or_else(|e| Completed::Failed {
            order_id: self.proposal.order_id,
            error: e,
        });

        self.complete(completed, ctx).await;
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let order_id = self.proposal.order_id;

        tracing::info!(
            %order_id,
            price = %self.proposal.price,
            "Received settlement proposal"
        );

        if let Err(error) = self.handle_proposal().await {
            self.complete(Completed::Failed { order_id, error }, ctx)
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

impl Actor {
    pub fn new(
        proposal: SettlementProposal,
        taker_id: Identity,
        connections: &(impl MessageChannel<maker_inc_connections::settlement::Response> + 'static),
        process_manager: xtra::Address<process_manager::Actor>,
        (on_stopping0, on_stopping1): (
            &(impl MessageChannel<Stopping<Self>> + 'static),
            &(impl MessageChannel<Stopping<Self>> + 'static),
        ),
        db: sqlx::SqlitePool,
    ) -> Self {
        Self {
            proposal,
            taker_id,
            connections: connections.clone_channel(),
            process_manager,
            on_stopping: vec![on_stopping0.clone_channel(), on_stopping1.clone_channel()],
            db,
        }
    }

    async fn handle_proposal(&mut self) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd(self.proposal.order_id, &mut conn).await?;

        let event = cfd.receive_collaborative_settlement_proposal(self.proposal.clone())?;
        self.process_manager
            .send(process_manager::Event::new(event))
            .await??;

        Ok(())
    }

    async fn complete(
        &mut self,
        completed: CollaborativeSettlementCompleted,
        ctx: &mut xtra::Context<Self>,
    ) {
        let order_id = self.proposal.order_id;
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
        }

        ctx.stop();
    }

    async fn accept(&mut self, ctx: &mut xtra::Context<Self>) -> Result<()> {
        let order_id = self.proposal.order_id;
        tracing::info!(%order_id,
                       "Settlement proposal accepted");

        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd(order_id, &mut conn).await?;
        let event = cfd.accept_collaborative_settlement_proposal(&self.proposal)?;

        self.process_manager
            .send(process_manager::Event::new(event))
            .await??;

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

        self.complete(Completed::rejected(order_id), ctx).await;
    }
}

impl ActorName for Actor {
    fn actor_name() -> String {
        "Maker collab settlement".to_string()
    }
}
