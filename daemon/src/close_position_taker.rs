use anyhow::{bail, Result};
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::Signature;
use futures::AsyncWriteExt;
use libp2p_core::PeerId;
use model::OrderId;
use model::Price;
use serde::Serialize;
use tokio_tasks::Tasks;
use xtra::Address;
use xtra_libp2p::Endpoint;
use xtra_libp2p::OpenSubstream;
use xtra_productivity::xtra_productivity;
use crate::bitcoin::Transaction;

pub struct Actor {
    endpoint: Address<Endpoint>,
    tasks: Tasks,
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
    pub async fn handle(&mut self, msg: ClosePosition, ctx: &mut xtra::Context<Self>) -> Result<()> {
        let this = ctx.address().expect("we are alive");
        let ClosePosition { id, price } = msg;

        let maker = PeerId::random(); // TODO: Load from CFD. Need to first save this to the DB before we can load it here.
        let taker_signature: Signature = todo!("preemptively sign close transaction");

        let mut substream = self
            .endpoint
            .send(OpenSubstream::single_protocol(
                maker,
                "/itchysats/close-position/1.0.0",
            ))
            .await??;

        self.tasks.add_fallible(
            async move {
                let msg0 = Msg0 { id, price };
                substream.write_all(&msg0.to_bytes()).await?;

                let msg1: Msg1 = todo!("Read Msg1 from substream, requires codec");

                if let Msg1::Reject = msg1 {
                    bail!("Listener rejected closing of position")
                }

                let msg2 = Msg2 {
                    dialer_signature: taker_signature
                };
                substream.write_all(&msg2.to_bytes()).await?;

                let msg3: Result<Msg3> = todo!("Read Msg3 from substream, requires codec");

                let msg3 = match msg3 {
                    Ok(msg3) => msg3,
                    Err(_) => {
                        bail!("Failed to receive signature") // TODO: This should probably be a typed error so we can handle it appropriately in the error handler (and partially complete the protocol). Perhaps use double result?
                    }
                };

                todo!("Verify signature!");

                let tx: Transaction = todo!("Assemble entire transaction from all signatures");

                this.send(FullyComplete {
                    tx
                }).await??;

                anyhow::Ok(())
            },
            |e| async move {},
        );

        Ok(())
    }

    pub async fn handle(&mut self, msg: FullyComplete, ctx: &mut xtra::Context<Self>) -> Result<()> {
        Ok(())
    }
}

/// Notify the actor about a fully-completed "close" protocol.
struct FullyComplete {
    tx: Transaction
}

#[derive(Serialize, Deserialize)]
pub struct Msg0 {
    pub id: OrderId,
    pub price: Price,
}

#[derive(Serialize, Deserialize)]
pub enum Msg1 {
    Accept,
    Reject
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
