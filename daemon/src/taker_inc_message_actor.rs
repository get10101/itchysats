use crate::{taker_cfd_actor, wire};
use futures::{Future, StreamExt};
use tokio::net::tcp::OwnedReadHalf;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, LengthDelimitedCodec};

pub fn new(
    read: OwnedReadHalf,
    cfd_actor: mpsc::UnboundedSender<taker_cfd_actor::Command>,
) -> impl Future<Output = ()> {
    let frame_read = FramedRead::new(read, LengthDelimitedCodec::new());

    let mut messages = frame_read.map(|result| {
        let message = serde_json::from_slice::<wire::MakerToTaker>(&result?)?;
        anyhow::Result::<_>::Ok(message)
    });

    async move {
        while let Some(message) = messages.next().await {
            match message {
                Ok(wire::MakerToTaker::CurrentOffer(offer)) => {
                    cfd_actor
                        .send(taker_cfd_actor::Command::NewOffer(offer))
                        .unwrap();
                }
                Ok(wire::MakerToTaker::ConfirmTakeOffer(offer_id)) => {
                    // TODO: This naming is not well aligned.
                    cfd_actor
                        .send(taker_cfd_actor::Command::OfferAccepted(offer_id))
                        .unwrap();
                }
                Err(error) => {
                    eprintln!("Error in reading message: {}", error);
                }
            }
        }
    }
}
