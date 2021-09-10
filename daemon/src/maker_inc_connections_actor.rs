use crate::{maker_cfd_actor, maker_inc_connections_actor, send_wire_message_actor, wire};
use futures::{Future, StreamExt};
use std::collections::HashMap;
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, LengthDelimitedCodec};
use uuid::Uuid;

pub enum Command {
    #[allow(dead_code)]
    NewOffer,
}

pub fn new(
    listener: TcpListener,
    cfd_maker_actor_inbox: mpsc::UnboundedSender<maker_cfd_actor::Command>,
    mut our_inbox: mpsc::UnboundedReceiver<maker_inc_connections_actor::Command>,
) -> impl Future<Output = ()> {
    let mut write_connections = HashMap::<Uuid, mpsc::UnboundedSender<wire::MakerToTaker>>::new();

    async move {
        loop {
            tokio::select! {
                Ok((socket, remote_addr)) = listener.accept() => {
                    println!("Connected to {}", remote_addr);
                    let uuid = Uuid::new_v4();
                    let (read, write) = socket.into_split();

                    let in_taker_actor = in_taker_messages(read, cfd_maker_actor_inbox.clone(), uuid);
                    let (out_message_actor, out_message_actor_inbox) = send_wire_message_actor::new(write);

                    tokio::spawn(in_taker_actor);
                    tokio::spawn(out_message_actor);

                    // TODO: Tell the CFD actor that there is a new taker available

                    write_connections.insert(uuid, out_message_actor_inbox);
                },
                Some(message) = our_inbox.recv() => {
                    match message {
                        maker_inc_connections_actor::Command::NewOffer => {
                            todo!("Broadcast to all takers")
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
    taker_id: Uuid,
) -> impl Future<Output = ()> {
    let mut messages = FramedRead::new(read, LengthDelimitedCodec::new()).map(|result| {
        let message = serde_json::from_slice::<wire::TakerToMaker>(&result?)?;
        anyhow::Result::<_>::Ok(message)
    });

    async move {
        while let Some(message) = messages.next().await {
            match message {
                Ok(wire::TakerToMaker::TakeOffer { offer_id, quantity }) => cfd_actor_inbox
                    .send(maker_cfd_actor::Command::TakeOffer {
                        taker_id,
                        offer_id,
                        quantity,
                    })
                    .unwrap(),
                Ok(wire::TakerToMaker::StartContractSetup(_offer_id)) => {}
                Err(error) => {
                    eprintln!("Error in reading message: {}", error);
                }
            }
        }
    }
}
