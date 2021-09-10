use futures::{Future, SinkExt};
use serde::Serialize;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedWrite, LengthDelimitedCodec};

pub fn new<T>(write: OwnedWriteHalf) -> (impl Future<Output = ()>, mpsc::UnboundedSender<T>)
where
    T: Serialize,
{
    let (sender, mut receiver) = mpsc::unbounded_channel::<T>();

    let actor = async move {
        let mut framed_write = FramedWrite::new(write, LengthDelimitedCodec::new());

        while let Some(message) = receiver.recv().await {
            match framed_write
                .send(serde_json::to_vec(&message).unwrap().into())
                .await
            {
                Ok(_) => {}
                Err(_) => {
                    eprintln!("TCP connection error");
                    break;
                }
            }
        }
    };

    (actor, sender)
}
