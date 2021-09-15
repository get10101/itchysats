use crate::model::cfd::{Order, OrderId};
use crate::model::TakerId;
use crate::wire::SetupMsg;
use crate::{maker_cfd_actor, maker_inc_connections_actor, send_wire_message_actor, wire};
use futures::{Future, StreamExt};
use std::collections::HashMap;
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, LengthDelimitedCodec};

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Command {
    BroadcastOrder(Option<Order>),
    SendOrder {
        order: Option<Order>,
        taker_id: TakerId,
    },
    NotifyInvalidOrderId {
        id: OrderId,
        taker_id: TakerId,
    },
    NotifyOrderAccepted {
        id: OrderId,
        taker_id: TakerId,
    },
    OutProtocolMsg {
        taker_id: TakerId,
        msg: SetupMsg,
    },
}

pub fn new(
    listener: TcpListener,
    cfd_maker_actor_inbox: mpsc::UnboundedSender<maker_cfd_actor::Command>,
    mut our_inbox: mpsc::UnboundedReceiver<maker_inc_connections_actor::Command>,
) -> impl Future<Output = ()> {
    let mut write_connections =
        HashMap::<TakerId, mpsc::UnboundedSender<wire::MakerToTaker>>::new();

    async move {
        loop {
            tokio::select! {
                Ok((socket, remote_addr)) = listener.accept() => {
                    println!("Connected to {}", remote_addr);
                    let taker_id = TakerId::default();
                    let (read, write) = socket.into_split();

                    let in_taker_actor = in_taker_messages(read, cfd_maker_actor_inbox.clone(), taker_id);
                    let (out_message_actor, out_message_actor_inbox) = send_wire_message_actor::new(write);

                    tokio::spawn(in_taker_actor);
                    tokio::spawn(out_message_actor);

                    cfd_maker_actor_inbox.send(maker_cfd_actor::Command::NewTakerOnline{id : taker_id}).unwrap();

                    write_connections.insert(taker_id, out_message_actor_inbox);
                },
                Some(message) = our_inbox.recv() => {
                    match message {
                        maker_inc_connections_actor::Command::NotifyInvalidOrderId { id, taker_id } => {
                            let conn = write_connections.get(&taker_id).expect("no connection to taker_id");
                            conn.send(wire::MakerToTaker::InvalidOrderId(id)).unwrap();
                        },
                        maker_inc_connections_actor::Command::BroadcastOrder(order) => {
                            for conn in write_connections.values() {
                                conn.send(wire::MakerToTaker::CurrentOrder(order.clone())).unwrap();
                            }
                        },
                        maker_inc_connections_actor::Command::SendOrder {order, taker_id} => {
                            let conn = write_connections.get(&taker_id).expect("no connection to taker_id");
                            conn.send(wire::MakerToTaker::CurrentOrder(order)).unwrap();
                        },
                        maker_inc_connections_actor::Command::NotifyOrderAccepted { id, taker_id } => {
                            let conn = write_connections.get(&taker_id).expect("no connection to taker_id");
                            conn.send(wire::MakerToTaker::ConfirmTakeOrder(id)).unwrap();
                        },
                        maker_inc_connections_actor::Command::OutProtocolMsg { taker_id, msg } => {
                            let conn = write_connections.get(&taker_id).expect("no connection to taker_id");
                            conn.send(wire::MakerToTaker::Protocol(msg)).unwrap();
                        }
                    }
                }
            }
        }
    }
}

fn in_taker_messages(
    read: OwnedReadHalf,
    cfd_actor_inbox: mpsc::UnboundedSender<maker_cfd_actor::Command>,
    taker_id: TakerId,
) -> impl Future<Output = ()> {
    let mut messages = FramedRead::new(read, LengthDelimitedCodec::new()).map(|result| {
        let message = serde_json::from_slice::<wire::TakerToMaker>(&result?)?;
        anyhow::Result::<_>::Ok(message)
    });

    async move {
        while let Some(message) = messages.next().await {
            match message {
                Ok(wire::TakerToMaker::TakeOrder { order_id, quantity }) => cfd_actor_inbox
                    .send(maker_cfd_actor::Command::TakeOrder {
                        taker_id,
                        order_id,
                        quantity,
                    })
                    .unwrap(),
                Ok(wire::TakerToMaker::StartContractSetup(order_id)) => cfd_actor_inbox
                    .send(maker_cfd_actor::Command::StartContractSetup { taker_id, order_id })
                    .unwrap(),
                Ok(wire::TakerToMaker::Protocol(msg)) => cfd_actor_inbox
                    .send(maker_cfd_actor::Command::IncProtocolMsg(msg))
                    .unwrap(),
                Err(error) => {
                    eprintln!("Error in reading message: {}", error);
                }
            }
        }
    }
}
