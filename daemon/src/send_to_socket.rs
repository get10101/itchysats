use crate::wire::{self, EncryptedJsonCodec};
use futures::SinkExt;
use serde::Serialize;
use snow::TransportState;
use std::fmt;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use tokio::net::tcp::OwnedWriteHalf;
use tokio_util::codec::FramedWrite;
use xtra::{Handler, Message};

pub struct Actor<T> {
    write: FramedWrite<OwnedWriteHalf, EncryptedJsonCodec<T>>,
}

impl<T> Actor<T> {
    pub fn new(write: OwnedWriteHalf, transport_state: Arc<Mutex<TransportState>>) -> Self {
        Self {
            write: FramedWrite::new(write, EncryptedJsonCodec::new(transport_state)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.write.into_inner().shutdown().await;
    }
}

#[async_trait::async_trait]
impl<T> Handler<T> for Actor<T>
where
    T: Message<Result = ()> + Serialize + fmt::Display + Sync,
{
    async fn handle(&mut self, message: T, ctx: &mut xtra::Context<Self>) {
        let message_name = message.to_string(); // send consumes the message, avoid a clone just in case it errors by getting the name here

        if let Err(e) = self.write.send(message).await {
            tracing::error!("Failed to write message {} to socket: {}", message_name, e);
            ctx.stop();
        }
    }
}

impl<T: 'static + Send> xtra::Actor for Actor<T> {}

impl xtra::Message for wire::MakerToTaker {
    type Result = ();
}

impl xtra::Message for wire::TakerToMaker {
    type Result = ();
}
