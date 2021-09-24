use crate::model::cfd::Origin;
use crate::{taker_cfd, wire};
use futures::{Future, StreamExt};
use tokio::net::tcp::OwnedReadHalf;
use tokio_util::codec::{FramedRead, LengthDelimitedCodec};
use xtra::prelude::*;

pub fn new(read: OwnedReadHalf, cfd_actor: Address<taker_cfd::Actor>) -> impl Future<Output = ()> {
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
                        .do_send_async(taker_cfd::NewOrder(order))
                        .await
                        .expect("actor to be always available");
                }
                Ok(wire::MakerToTaker::ConfirmOrder(order_id)) => {
                    // TODO: This naming is not well aligned.
                    cfd_actor
                        .do_send_async(taker_cfd::OrderAccepted(order_id))
                        .await
                        .expect("actor to be always available");
                }
                Ok(wire::MakerToTaker::RejectOrder(order_id)) => {
                    cfd_actor
                        .do_send_async(taker_cfd::OrderRejected(order_id))
                        .await
                        .expect("actor to be always available");
                }
                Ok(wire::MakerToTaker::InvalidOrderId(_)) => {
                    todo!()
                }
                Ok(wire::MakerToTaker::Protocol(msg)) => {
                    cfd_actor
                        .do_send_async(taker_cfd::IncProtocolMsg(msg))
                        .await
                        .expect("actor to be always available");
                }
                Err(error) => {
                    tracing::error!("Error in reading message: {}", error);
                }
            }
        }
    }
}
