use crate::address_map::{ActorName, Stopping};
use crate::model::cfd::{
    Cfd, CollaborativeSettlement, OrderId, Role, SettlementKind, SettlementProposal,
};
use crate::model::Identity;
use crate::{maker_inc_connections, projection};
use anyhow::Context;
use async_trait::async_trait;
use bdk::bitcoin::Script;
use maia::secp256k1_zkp::Signature;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    cfd: Cfd,
    projection: xtra::Address<projection::Actor>,
    on_completed: Box<dyn MessageChannel<Completed>>,
    proposal: SettlementProposal,
    taker_id: Identity,
    connections: Box<dyn MessageChannel<maker_inc_connections::settlement::Response>>,
    on_stopping: Vec<Box<dyn MessageChannel<Stopping<Self>>>>,
}

pub enum Completed {
    Confirmed {
        order_id: OrderId,
        settlement: CollaborativeSettlement,
        script_pubkey: Script,
    },
    Rejected {
        order_id: OrderId,
    },
    Failed {
        order_id: OrderId,
        error: anyhow::Error,
    },
}

pub struct Accepted;
pub struct Rejected;
pub struct Initiated {
    pub sig_taker: Signature,
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: Accepted, ctx: &mut xtra::Context<Self>) {
        let order_id = self.cfd.order.id;

        tracing::info!(%order_id, "Settlement proposal accepted");

        self.accept(ctx).await;
        self.update_proposal(None).await;
    }

    async fn handle(&mut self, _: Rejected, ctx: &mut xtra::Context<Self>) {
        let order_id = self.cfd.order.id;

        tracing::info!(%order_id, "Settlement proposal rejected");

        self.reject(ctx).await;
        self.update_proposal(None).await;
    }

    async fn handle(&mut self, msg: Initiated, ctx: &mut xtra::Context<Self>) {
        let completed = async {
            tracing::info!(
                order_id = %self.cfd.order.id,
                taker_id = %self.taker_id,
                "Received signature for collaborative settlement"
            );

            let Initiated { sig_taker } = msg;

            let dlc = self.cfd.open_dlc().context("CFD was in wrong state")?;
            let (tx, sig_maker) = dlc.close_transaction(&self.proposal)?;
            let spend_tx = dlc.finalize_spend_transaction((tx, sig_maker), sig_taker)?;

            let settlement = CollaborativeSettlement::new(
                spend_tx.clone(),
                dlc.script_pubkey_for(Role::Maker),
                self.proposal.price,
            )?;

            self.update_proposal(None).await;

            anyhow::Ok(Completed::Confirmed {
                order_id: self.cfd.order.id,
                settlement,
                script_pubkey: dlc.script_pubkey_for(Role::Maker),
            })
        }
        .await
        .unwrap_or_else(|e| Completed::Failed {
            order_id: self.cfd.order.id,
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
}

impl Actor {
    pub fn new(
        cfd: Cfd,
        proposal: SettlementProposal,
        projection: xtra::Address<projection::Actor>,
        on_completed: &(impl MessageChannel<Completed> + 'static),
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
                order: self.cfd.order.id,
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

    async fn complete(&mut self, completed: Completed, ctx: &mut xtra::Context<Self>) {
        let _ = self.on_completed.send(completed).await;

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
        let order_id = self.cfd.order.id;

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
