use crate::wire::{self, JsonCodec};
use futures::SinkExt;
use serde::Serialize;
use std::fmt;
use tokio::io::AsyncWriteExt;
use tokio::net::tcp::OwnedWriteHalf;
use tokio_util::codec::FramedWrite;
use xtra::{Handler, Message};

#[derive(xtra_productivity::Actor)]
pub struct Actor<T> {
    write: FramedWrite<OwnedWriteHalf, JsonCodec<T>>,
}

impl<T> Actor<T> {
    pub fn new(write: OwnedWriteHalf) -> Self {
        Self {
            write: FramedWrite::new(write, JsonCodec::default()),
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

impl xtra::Message for wire::MakerToTaker {
    type Result = ();
}

impl xtra::Message for wire::TakerToMaker {
    type Result = ();
}
