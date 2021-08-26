use serde::{Deserialize, Serialize};
use tokio::net::tcp::OwnedWriteHalf;

use crate::model::cfd::{CfdOffer, CfdOfferId, CfdTakeRequest};

use futures::SinkExt;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::codec::{FramedWrite, LengthDelimitedCodec};

// TODO: Implement messages used for confirming CFD offer between maker and taker
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum Message {
    CurrentOffer(Option<CfdOffer>),
    TakeOffer(CfdTakeRequest),
    // TODO: Needs RejectOffer as well
    ConfirmTakeOffer(CfdOfferId),
    // TODO: Currently the taker starts, can already send some stuff for signing over in the first message.
    StartContractSetup(CfdOfferId),
}

pub fn spawn_sender(write: OwnedWriteHalf) -> UnboundedSender<Message> {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Message>();
    tokio::spawn(async move {
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
    });
    sender
}
