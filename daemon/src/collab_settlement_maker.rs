use crate::address_map::ActorName;
use crate::address_map::Stopping;
use crate::maker_inc_connections;
use crate::model::cfd::Cfd;
use crate::model::cfd::CollaborativeSettlementCompleted;
use crate::model::cfd::Completed;
use crate::model::cfd::SettlementKind;
use crate::model::cfd::SettlementProposal;
use crate::model::Identity;
use crate::projection;
use crate::xtra_ext::LogFailure;
use anyhow::Context;
use async_trait::async_trait;
use maia::secp256k1_zkp::Signature;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    cfd: Cfd,
    projection: xtra::Address<projection::Actor>,
    on_completed: Box<dyn MessageChannel<CollaborativeSettlementCompleted>>,
    proposal: SettlementProposal,
    taker_id: Identity,
    connections: Box<dyn MessageChannel<maker_inc_connections::settlement::Response>>,
    on_stopping: Vec<Box<dyn MessageChannel<Stopping<Self>>>>,
}

pub struct Accepted;
pub struct Rejected;
pub struct Initiated {
    pub sig_taker: Signature,
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: Accepted, ctx: &mut xtra::Context<Self>) {
        let order_id = self.cfd.id();

        tracing::info!(%order_id, "Settlement proposal accepted");

        self.accept(ctx).await;
        self.update_proposal(None).await;
    }

    async fn handle(&mut self, _: Rejected, ctx: &mut xtra::Context<Self>) {
        let order_id = self.cfd.id();

        tracing::info!(%order_id, "Settlement proposal rejected");

        self.reject(ctx).await;
        self.update_proposal(None).await;
    }

    async fn handle(&mut self, msg: Initiated, ctx: &mut xtra::Context<Self>) {
        let completed = async {
            tracing::info!(
                order_id = %self.cfd.id(),
                taker_id = %self.taker_id,
                "Received signature for collaborative settlement"
            );

            let settlement = self
                .cfd
                .start_collaborative_settlement_maker(self.proposal.clone(), msg.sig_taker)?;

            self.update_proposal(None).await;

            anyhow::Ok(Completed::Succeeded {
                order_id: self.cfd.id(),
                payload: settlement,
            })
        }
        .await
        .unwrap_or_else(|e| Completed::Failed {
            order_id: self.cfd.id(),
            error: e,
        });

        self.complete(completed, ctx).await;
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    async fn started(&mut self, _ctx: &mut xtra::Context<Self>) {
        tracing::info!(
            order_id = %self.proposal.order_id,
            price = %self.proposal.price,
            "Received settlement proposal"
        );

        self.update_proposal(Some((self.proposal.clone(), SettlementKind::Incoming)))
            .await;
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

    async fn stopped(mut self) {
        let _ = self.update_proposal(None).await;
    }
}

impl Actor {
    pub fn new(
        cfd: Cfd,
        proposal: SettlementProposal,
        projection: xtra::Address<projection::Actor>,
        on_completed: &(impl MessageChannel<CollaborativeSettlementCompleted> + 'static),
        taker_id: Identity,
        connections: &(impl MessageChannel<maker_inc_connections::settlement::Response> + 'static),
        (on_stopping0, on_stopping1): (
            &(impl MessageChannel<Stopping<Self>> + 'static),
            &(impl MessageChannel<Stopping<Self>> + 'static),
        ),
    ) -> Self {
        Self {
            cfd,
            projection,
            on_completed: on_completed.clone_channel(),
            proposal,
            taker_id,
            connections: connections.clone_channel(),
            on_stopping: vec![on_stopping0.clone_channel(), on_stopping1.clone_channel()],
        }
    }

    async fn update_proposal(&mut self, proposal: Option<(SettlementProposal, SettlementKind)>) {
        if let Err(e) = self
            .projection
            .send(projection::UpdateSettlementProposal {
                order: self.cfd.id(),
                proposal,
            })
            .await
        {
            tracing::warn!(
                "Failed to deliver settlement proposal update to projection actor: {:#}",
                e
            );
        };
    }

    async fn complete(
        &mut self,
        completed: CollaborativeSettlementCompleted,
        ctx: &mut xtra::Context<Self>,
    ) {
        let _ = self
            .on_completed
            .send(completed)
            .log_failure("Failed to inform about collab settlement completion")
            .await;

        ctx.stop();
    }

    async fn accept(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("self to be alive");
        self.inform_taker(
            maker_inc_connections::settlement::Decision::Accept { address: this },
            ctx,
        )
        .await
    }

    async fn reject(&mut self, ctx: &mut xtra::Context<Self>) {
        self.inform_taker(maker_inc_connections::settlement::Decision::Reject, ctx)
            .await
    }

    async fn inform_taker(
        &mut self,
        decision: maker_inc_connections::settlement::Decision,
        ctx: &mut xtra::Context<Self>,
    ) {
        let order_id = self.cfd.id();

        if let Err(e) = self
            .connections
            .send(maker_inc_connections::settlement::Response {
                taker_id: self.taker_id,
                order_id,
                decision,
            })
            .await
            .context("Failed inform taker about settlement decision")
        {
            self.complete(Completed::Failed { order_id, error: e }, ctx)
                .await;
        }
    }
}

impl ActorName for Actor {
    fn actor_name() -> String {
        "Maker collab settlement".to_string()
    }
}
