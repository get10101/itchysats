use crate::wire;
use futures::stream::SplitSink;
use futures::SinkExt;
use serde::Serialize;
use std::fmt;
use tokio::net::TcpStream;
use tokio_util::codec::Framed;
use xtra::{Handler, Message};

pub struct Actor<D, E> {
    write: SplitSink<Framed<TcpStream, wire::EncryptedJsonCodec<D, E>>, E>,
}

impl<D, E> Actor<D, E> {
    pub fn new(write: SplitSink<Framed<TcpStream, wire::EncryptedJsonCodec<D, E>>, E>) -> Self {
        Self { write }
    }
}

#[async_trait::async_trait]
impl<D, E> Handler<E> for Actor<D, E>
where
    D: Send + Sync + 'static,
    E: Message<Result = ()> + Serialize + fmt::Display + Sync,
{
    async fn handle(&mut self, message: E, ctx: &mut xtra::Context<Self>) {
        let message_name = message.to_string(); // send consumes the message, avoid a clone just in case it errors by getting the name here

        tracing::trace!("Sending '{}'", message_name);

        if let Err(e) = self.write.send(message).await {
            tracing::error!("Failed to write message {} to socket: {}", message_name, e);
            ctx.stop();
        }
    }
}

impl<D, E> xtra::Actor for Actor<D, E>
where
    D: 'static + Send,
    E: 'static + Send,
{
}

impl xtra::Message for wire::MakerToTaker {
    type Result = ();
}

impl xtra::Message for wire::TakerToMaker {
    type Result = ();
}
