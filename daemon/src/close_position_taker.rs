use crate::bitcoin::Transaction;
use crate::command;
use anyhow::anyhow;
use anyhow::Context as _;
use anyhow::Error;
use anyhow::Result;
use async_trait::async_trait;
use asynchronous_codec::Decoder;
use bdk::bitcoin::secp256k1::Signature;
use bytes::BytesMut;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::PeerId;
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

        let (proposal, unsigned_tx, taker_signature, script) = self
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
                let mut framed = asynchronous_codec::Framed::new(substream, asynchronous_codec::JsonCodec::<DialerMessage, ListenerMessage>::new());

                framed.send(DialerMessage::Msg0(Msg0 { id, price })).await.context("Failed to send Msg0")?;

                // TODO: We will need to apply a timeout to these. Perhaps we can put a timeout generally into "reading from the substream"?
                let msg1 = framed.next().await.context("End of stream while receiving Msg1")?.context("Failed to decode Msg1")?.into_msg1()?;

                if let Msg1::Reject = msg1 {
                    return Err(Failed::Rejected);
                }

                framed.send(DialerMessage::Msg2(Msg2 {
                    dialer_signature: taker_signature,
                })).await.context("Failed to send Msg2")?;

                let msg3 = match framed.next().await {
                    Some(Ok(msg)) => msg.into_msg3()?,
                    Some(Err(_)) | None => {
                        return Err(Failed::AfterSendingSignature { unsigned_tx });
                    }
                };

                todo!("Verify signature!");

                let tx: Transaction = todo!("Assemble entire transaction from all signatures");

                fully_complete_mc.send(FullyComplete { tx }).await.expect("TODO: How to handle actor being disconnected after having a fully-signed transaction???");

                Ok(())
            },
            |e| async move {
                match e {
                    Failed::AfterSendingSignature { unsigned_tx: tx } => {
                        let _ = this.send(PartiallyComplete { tx }).await;
                    }
                    Failed::BeforeSendingSignature { source } => {
                        let _ = this.send(NotComplete { error: source }).await;
                    }
                    Failed::Rejected => {
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
    tx: Transaction, /* TODO: We don't have access to the `Dlc` model inside the protocol so
                      * likely, we want to return the individual building blocks here. Or
                      * refactor things so we can access it? */
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
    Rejected,
    AfterSendingSignature { unsigned_tx: Transaction },
    BeforeSendingSignature { source: anyhow::Error },
}

impl From<anyhow::Error> for Failed {
    fn from(source: Error) -> Self {
        Self::BeforeSendingSignature { source }
    }
}

#[derive(Serialize, Deserialize)]
enum DialerMessage {
    Msg0(Msg0),
    Msg2(Msg2),
}

#[derive(Serialize, Deserialize)]
enum ListenerMessage {
    Msg1(Msg1),
    Msg3(Msg3),
}

impl ListenerMessage {
    fn into_msg1(self) -> Result<Msg1> {
        match self {
            ListenerMessage::Msg1(msg1) => Ok(msg1),
            ListenerMessage::Msg3(_) => Err(anyhow!("Expected Msg1 but got Msg3")),
        }
    }

    fn into_msg3(self) -> Result<Msg3> {
        match self {
            ListenerMessage::Msg3(msg3) => Ok(msg3),
            ListenerMessage::Msg1(_) => Err(anyhow!("Expected Msg3 but got Msg1")),
        }
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

#[derive(Serialize, Deserialize)]
pub struct Msg3 {
    pub listener_signature: Signature,
}
