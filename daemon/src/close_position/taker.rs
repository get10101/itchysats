use crate::bitcoin::Transaction;
use crate::close_position::protocol::*;
use crate::command;
use anyhow::anyhow;
use anyhow::Result;
use async_trait::async_trait;
use libp2p_core::PeerId;
use model::CollaborativeSettlement;
use model::OrderId;
use model::Price;
use tokio_tasks::Tasks;
use xtra::Address;
use xtra_libp2p::Endpoint;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    endpoint: Address<Endpoint>,
    tasks: Tasks,
    executor: command::Executor,
    n_payouts: usize, // TODO: Consider hard-coding this in the model crate.
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

pub struct ClosePosition {
    pub id: OrderId,
    pub price: Price,
}

#[xtra_productivity]
impl Actor {
    pub async fn handle(
        &mut self,
        msg: ClosePosition,
        ctx: &mut xtra::Context<Self>,
    ) -> Result<()> {
        let this = ctx.address().expect("we are alive");

        let ClosePosition { id, price } = msg;

        let close_position_tx = self
            .executor
            .execute(id, |cfd| {
                cfd.start_close_position_taker(price, self.n_payouts)
            })
            .await?;

        let maker = PeerId::random(); // TODO: This needs to be returned from the above `execute` call as well.

        let endpoint = self.endpoint.clone();

        // TODO: I think this part could live in a separate crate?
        self.tasks.add_fallible(
            {
                let this = this.clone();
                async move {
                    let settlement = dialer(endpoint, id, maker, close_position_tx).await?;
                    let _ = this.send(FullyComplete { settlement }).await.expect("TODO: How to handle actor being disconnected after having a fully-signed transaction???");

                    Ok(())
                }
            },
            |e| async move {
                match e {
                    DialerFailed::AfterSendingSignature { unsigned_tx: tx } => {
                        let _ = this.send(PartiallyComplete { tx }).await;
                    }
                    DialerFailed::BeforeSendingSignature { source } => {
                        let _ = this.send(NotComplete { error: source }).await;
                    }
                    DialerFailed::Rejected => {
                        let _ = this.send(NotComplete { error: anyhow!("Rejected") }).await;
                    }
                }
            },
        );

        Ok(())
    }

    pub async fn handle(
        &mut self,
        msg: FullyComplete,
        ctx: &mut xtra::Context<Self>,
    ) -> Result<()> {
        Ok(())
    }

    pub async fn handle(
        &mut self,
        msg: PartiallyComplete,
        ctx: &mut xtra::Context<Self>,
    ) -> Result<()> {
        Ok(())
    }

    pub async fn handle(&mut self, msg: NotComplete, ctx: &mut xtra::Context<Self>) -> Result<()> {
        Ok(())
    }
}

/// Notify the actor about a fully-completed "close" protocol.
struct FullyComplete {
    settlement: CollaborativeSettlement,
}

/// Notify the actor about a partially-completed "close" protocol.
struct PartiallyComplete {
    tx: Transaction,
}

/// Notify the actor about a not-completed "close" protocol.
struct NotComplete {
    error: anyhow::Error,
}
