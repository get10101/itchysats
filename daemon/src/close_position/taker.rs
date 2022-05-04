use crate::bitcoin::Transaction;
use crate::close_position::protocol::*;
use crate::close_position::PROTOCOL;
use crate::command;
use crate::Amount;
use anyhow::anyhow;
use anyhow::Context as _;
use anyhow::Error;
use anyhow::Result;
use async_trait::async_trait;
use asynchronous_codec::Decoder;
use bdk::bitcoin::secp256k1::Signature;
use bdk::bitcoin::PublicKey;
use bdk::miniscript::Descriptor;
use bytes::BytesMut;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::PeerId;
use model::CollaborativeSettlement;
use model::OrderId;
use model::Price;
use serde::Deserialize;
use serde::Serialize;
use tokio_tasks::Tasks;
use xtra::message_channel::StrongMessageChannel;
use xtra::Address;
use xtra_libp2p::Endpoint;
use xtra_libp2p::OpenSubstream;
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
                    let settlement = dialer(endpoint, maker, close_position_tx).await?;
                    this.send(FullyComplete { settlement }).await.expect("TODO: How to handle actor being disconnected after having a fully-signed transaction???");

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
