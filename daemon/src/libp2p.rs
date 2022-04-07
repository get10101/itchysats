use anyhow::Context as _;
use anyhow::Result;
use async_trait::async_trait;
use asynchronous_codec::Bytes;
use futures::SinkExt;
use futures::StreamExt;
use tokio_tasks::Tasks;

use libp2p_xtra::NewInboundSubstream;
use xtra::prelude::*;
use xtra_productivity::xtra_productivity;

#[derive(Default)]
pub struct HelloWorld {
    tasks: Tasks,
}

#[xtra_productivity(message_impl = false)]
impl HelloWorld {
    async fn handle(&mut self, msg: NewInboundSubstream) {
        tracing::info!("New hello world stream from {}", msg.peer);

        self.tasks
            .add_fallible(hello_world_listener(msg.stream), move |e| async move {
                tracing::warn!("Hello world protocol with peer {} failed: {}", msg.peer, e);
            });
    }
}

#[async_trait]
impl Actor for HelloWorld {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

async fn hello_world_dialer(stream: libp2p_xtra::Substream, name: &'static str) -> Result<String> {
    let mut stream = asynchronous_codec::Framed::new(stream, asynchronous_codec::LengthCodec);

    stream.send(Bytes::from(name)).await?;
    let bytes = stream.next().await.context("Expected message")??;
    let message = String::from_utf8(bytes.to_vec())?;

    Ok(message)
}

pub async fn hello_world_listener(stream: libp2p_xtra::Substream) -> Result<()> {
    let mut stream =
        asynchronous_codec::Framed::new(stream, asynchronous_codec::LengthCodec).fuse();

    let bytes = stream.select_next_some().await?;
    let name = String::from_utf8(bytes.to_vec())?;

    stream.send(Bytes::from(format!("Hello {name}!"))).await?;

    Ok(())
}
