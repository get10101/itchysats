use crate::model::cfd::Origin;
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
                Ok(wire::MakerToTaker::CurrentOrder(mut order)) => {
                    if let Some(order) = order.as_mut() {
                        order.origin = Origin::Theirs;
                    }

                    cfd_actor
                        .send(taker_cfd_actor::Command::NewOrder(order))
                        .unwrap();
                }
                Ok(wire::MakerToTaker::ConfirmOrder(order_id)) => {
                    // TODO: This naming is not well aligned.
                    cfd_actor
                        .send(taker_cfd_actor::Command::OrderAccepted(order_id))
                        .unwrap();
                }
                Ok(wire::MakerToTaker::RejectOrder(order_id)) => {
                    cfd_actor
                        .send(taker_cfd_actor::Command::OrderRejected(order_id))
                        .unwrap();
                }
                Ok(wire::MakerToTaker::InvalidOrderId(_)) => {
                    todo!()
                }
                Ok(wire::MakerToTaker::Protocol(msg)) => {
                    cfd_actor
                        .send(taker_cfd_actor::Command::IncProtocolMsg(msg))
                        .unwrap();
                }
                Err(error) => {
                    tracing::error!("Error in reading message: {}", error);
                }
            }
        }
    }
}
