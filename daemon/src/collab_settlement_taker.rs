use crate::address_map::{ActorName, Stopping};
use crate::model::cfd::{
    Cfd, CollaborativeSettlement, OrderId, SettlementKind, SettlementProposal,
};
use crate::model::Price;
use crate::{connection, projection, wire};
use anyhow::{Context, Result};
use async_trait::async_trait;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    cfd: Cfd,
    projection: xtra::Address<projection::Actor>,
    on_completed: Box<dyn MessageChannel<Completed>>,
    connection: xtra::Address<connection::Actor>,
    proposal: SettlementProposal,
}

impl Actor {
    pub fn new(
        cfd: Cfd,
        projection: xtra::Address<projection::Actor>,
        on_completed: impl MessageChannel<Completed> + 'static,
        current_price: Price,
        connection: xtra::Address<connection::Actor>,
        n_payouts: usize,
    ) -> Result<Self> {
        let proposal = cfd.calculate_settlement(current_price, n_payouts)?;

        Ok(Self {
            cfd,
            projection,
            on_completed: Box::new(on_completed),
            connection,
            proposal,
        })
    }

    async fn propose(&mut self, this: xtra::Address<Self>) -> Result<()> {
        if !self.cfd.is_collaborative_settle_possible() {
            anyhow::bail!(
                "Settlement proposal not possible because for cfd {} is in state {} which cannot be collaboratively settled",
                self.cfd.id,
                self.cfd.state
            )
        }

        self.connection
            .send(connection::ProposeSettlement {
                timestamp: self.proposal.timestamp,
                taker: self.proposal.taker,
                maker: self.proposal.maker,
                price: self.proposal.price,
                address: this,
                order_id: self.cfd.id,
            })
            .await??;

        self.update_proposal(Some((self.proposal.clone(), SettlementKind::Outgoing)))
            .await?;

        Ok(())
    }

    async fn handle_confirmed(&mut self) -> Result<CollaborativeSettlement> {
        let order_id = self.cfd.id;

        tracing::info!(%order_id, "Settlement proposal got accepted");

        self.update_proposal(None).await?;

        let dlc = self.cfd.dlc().context("No DLC in CFD")?;

        let (tx, sig) = dlc.close_transaction(&self.proposal)?;

        // Need to use `do_send_async` here because this handler is called in
        // context of a message arriving over the wire, and would result in a
        // deadlock otherwise.
        #[allow(clippy::disallowed_method)]
        self.connection
            .do_send_async(wire::TakerToMaker::Settlement {
                order_id,
                msg: wire::taker_to_maker::Settlement::Initiate { sig_taker: sig },
            })
            .await?;

        Ok(CollaborativeSettlement::new(
            tx,
            dlc.script_pubkey_for(self.cfd.role()), // TODO: Hardcode role to Taker?
            self.proposal.price,
        )?)
    }

    async fn handle_rejected(&mut self) -> Result<()> {
        let order_id = self.cfd.id;

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
                order: self.cfd.id,
                proposal,
            })
            .await?;

        Ok(())
    }

    async fn complete(&mut self, completed: Completed, ctx: &mut xtra::Context<Self>) {
        let _ = self.on_completed.send(completed).await;

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
                    order_id: self.cfd.id,
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
}

pub enum Completed {
    Confirmed {
        order_id: OrderId,
        settlement: CollaborativeSettlement,
    },
    Rejected {
        order_id: OrderId,
    },
    Failed {
        order_id: OrderId,
        error: anyhow::Error,
    },
}

#[xtra_productivity]
impl Actor {
    async fn handle(
        &mut self,
        msg: wire::maker_to_taker::Settlement,
        ctx: &mut xtra::Context<Self>,
    ) {
        let order_id = self.cfd.id;

        let completed = match msg {
            wire::maker_to_taker::Settlement::Confirm => match self.handle_confirmed().await {
                Ok(settlement) => Completed::Confirmed {
                    settlement,
                    order_id,
                },
                Err(e) => Completed::Failed { error: e, order_id },
            },
            wire::maker_to_taker::Settlement::Reject => {
                if let Err(e) = self.handle_rejected().await {
                    Completed::Failed { error: e, order_id }
                } else {
                    Completed::Rejected { order_id }
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
