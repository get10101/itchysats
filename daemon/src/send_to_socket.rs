use futures::SinkExt;
use serde::Serialize;
use std::fmt;
use tokio::net::tcp::OwnedWriteHalf;
use tokio_util::codec::{FramedWrite, LengthDelimitedCodec};
use xtra::{Handler, Message};

pub struct Actor {
    write: FramedWrite<OwnedWriteHalf, LengthDelimitedCodec>,
}

impl Actor {
    pub fn new(write: OwnedWriteHalf) -> Self {
        Self {
            write: FramedWrite::new(write, LengthDelimitedCodec::new()),
        }
    }
}

#[async_trait::async_trait]
impl<T> Handler<T> for Actor
where
    T: Message<Result = ()> + Serialize + fmt::Display,
{
    async fn handle(&mut self, message: T, ctx: &mut xtra::Context<Self>) {
        let bytes = serde_json::to_vec(&message).expect("serialization should never fail");

        if let Err(e) = self.write.send(bytes.into()).await {
            tracing::error!("Failed to write message {} to socket: {}", message, e);
            ctx.stop();
        }
    }
}

impl xtra::Actor for Actor {}
