use crate::bitcoin::Transaction;
use crate::command;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context as _;
use anyhow::Error;
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::Signature;
use futures::AsyncWriteExt;
use libp2p_core::PeerId;
use model::OrderId;
use model::Price;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;
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

        let (proposal, transaction, taker_signature, script) = self
            .executor
            .execute(id, |cfd| {
                cfd.propose_collaborative_settlement(price, self.n_payouts)
            })
            .await?;

        let maker = PeerId::random(); // TODO: This needs to be returned from the above `execute` call as well.

        let endpoint = self.endpoint.clone();
        let fully_complete_mc = this.clone_channel();

        // TODO: I think this part could live in a separate crate?
        self.tasks.add_fallible(
            async move {
                let mut substream = endpoint
                    .send(OpenSubstream::single_protocol(
                        maker,
                        "/itchysats/close-position/1.0.0",
                    ))
                    .await
                    .context("Endpoint is disconnected")?
                    .context("Failed to open substream")?;

                let msg0 = Msg0 { id, price };
                substream
                    .write_all(&msg0.to_bytes())
                    .await
                    .context("Failed to send Msg0")?;

                let msg1: Msg1 = todo!("Read Msg1 from substream, requires codec");

                if let Msg1::Reject = msg1 {
                    return Err(Failed::BeforeSendingSignature {
                        source: anyhow!("Maker rejected closing of position"), /* TODO: This
                                                                                * should probably
                                                                                * be its own
                                                                                * variant. */
                    });
                }

                let msg2 = Msg2 {
                    dialer_signature: taker_signature,
                };
                substream.write_all(&msg2.to_bytes()).await.context("Failed to send Msg2")?;

                let msg3: Result<Msg3> = todo!("Read Msg3 from substream, requires codec");

                let msg3 = match msg3 {
                    Ok(msg3) => msg3,
                    Err(_) => {
                        // TODO: Add our own signature to `tx`

                        return Err(Failed::AfterSendingSignature { tx: transaction });
                    }
                };

                todo!("Verify signature!");

                let tx: Transaction = todo!("Assemble entire transaction from all signatures");

                fully_complete_mc.send(FullyComplete { tx }).await.expect("TODO: How to handle actor being disconnected after having a fully-signed transaction???");

                Ok(())
            },
            |e| async move {
                match e {
                    Failed::AfterSendingSignature { tx } => {
                        let _ = this.send(PartiallyComplete { tx }).await;
                    }
                    Failed::BeforeSendingSignature { source } => {
                        let _ = this.send(NotComplete { error: source }).await;
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
    tx: Transaction,
}

/// Notify the actor about a partially-completed "close" protocol.
struct PartiallyComplete {
    tx: Transaction,
}

/// Notify the actor about a not-completed "close" protocol.
struct NotComplete {
    error: anyhow::Error,
}

#[derive(Debug)]
enum Failed {
    AfterSendingSignature { tx: Transaction },
    BeforeSendingSignature { source: anyhow::Error },
}

impl From<anyhow::Error> for Failed {
    fn from(source: Error) -> Self {
        Self::BeforeSendingSignature { source }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Msg0 {
    pub id: OrderId,
    pub price: Price,
}

#[derive(Serialize, Deserialize)]
pub enum Msg1 {
    Accept,
    Reject,
}

#[derive(Serialize, Deserialize)]
pub struct Msg2 {
    pub dialer_signature: Signature,
}

impl Msg2 {
    fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serialization always succeeds")
    }
}

#[derive(Serialize, Deserialize)]
pub struct Msg3 {
    pub listener_signature: Signature,
}

impl Msg0 {
    fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serialization always succeeds")
    }
}
