use crate::maker_cfd::{FromTaker, NewTakerOnline};
use crate::model::cfd::{Order, OrderId};
use crate::model::{BitMexPriceEventId, TakerId};
use crate::tokio_ext::FutureExt;
use crate::{forward_only_ok, maker_cfd, noise, send_to_socket, wire, HEARTBEAT_INTERVAL};
use anyhow::Result;
use futures::future::RemoteHandle;
use futures::{StreamExt, TryStreamExt};
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpStream;
use tokio_util::codec::FramedRead;
use xtra::prelude::*;
use xtra::{Actor as _, KeepRunning};
use xtra_productivity::xtra_productivity;

pub struct BroadcastOrder(pub Option<Order>);

#[allow(clippy::large_enum_variant)]
pub enum TakerCommand {
    SendOrder {
        order: Option<Order>,
    },
    NotifyInvalidOrderId {
        id: OrderId,
    },
    NotifyOrderAccepted {
        id: OrderId,
    },
    NotifyOrderRejected {
        id: OrderId,
    },
    NotifySettlementAccepted {
        id: OrderId,
    },
    NotifySettlementRejected {
        id: OrderId,
    },
    NotifyRollOverAccepted {
        id: OrderId,
        oracle_event_id: BitMexPriceEventId,
    },
    NotifyRollOverRejected {
        id: OrderId,
    },
    Protocol(wire::SetupMsg),
    RollOverProtocol(wire::RollOverMsg),
}

pub struct TakerMessage {
    pub taker_id: TakerId,
    pub command: TakerCommand,
}

pub enum ListenerMessage {
    NewConnection {
        stream: TcpStream,
        address: SocketAddr,
    },
    Error {
        source: io::Error,
    },
}

pub struct Actor {
    write_connections: HashMap<TakerId, Address<send_to_socket::Actor<wire::MakerToTaker>>>,
    new_taker_channel: Box<dyn MessageChannel<NewTakerOnline>>,
    taker_msg_channel: Box<dyn MessageChannel<FromTaker>>,
    noise_priv_key: x25519_dalek::StaticSecret,
    tasks: Vec<RemoteHandle<()>>,
}

impl Actor {
    pub fn new(
        new_taker_channel: Box<dyn MessageChannel<NewTakerOnline>>,
        taker_msg_channel: Box<dyn MessageChannel<FromTaker>>,
        noise_priv_key: x25519_dalek::StaticSecret,
    ) -> Self {
        Self {
            write_connections: HashMap::new(),
            new_taker_channel: new_taker_channel.clone_channel(),
            taker_msg_channel: taker_msg_channel.clone_channel(),
            noise_priv_key,
            tasks: Vec::new(),
        }
    }

    async fn send_to_taker(
        &mut self,
        taker_id: &TakerId,
        msg: wire::MakerToTaker,
    ) -> Result<(), NoConnection> {
        let conn = self
            .write_connections
            .get(taker_id)
            .ok_or_else(|| NoConnection(*taker_id))?;

        let msg_str = msg.to_string();

        if conn.send(msg).await.is_err() {
            tracing::info!(%taker_id, "Failed to send {} to taker, removing connection", msg_str);
            self.write_connections.remove(taker_id);
        }

        Ok(())
    }

    async fn handle_new_connection_impl(
        &mut self,
        mut stream: TcpStream,
        taker_address: SocketAddr,
        _: &mut Context<Self>,
    ) -> Result<()> {
        let taker_id = TakerId::default();

        tracing::info!("New taker {} connected on {}", taker_id, taker_address);

        let noise = Arc::new(Mutex::new(
            noise::responder_handshake(&mut stream, &self.noise_priv_key).await?,
        ));

        let (read, write) = stream.into_split();
        let read = FramedRead::new(read, wire::EncryptedJsonCodec::new(noise.clone()))
            .map_ok(move |msg| FromTaker { taker_id, msg })
            .map(forward_only_ok::Message);

        let (out_msg_actor_address, mut out_msg_actor_context) = xtra::Context::new(None);

        let (forward_to_cfd, forward_to_cfd_fut) =
            forward_only_ok::Actor::new(self.taker_msg_channel.clone_channel())
                .create(None)
                .run();
        self.tasks.push(forward_to_cfd_fut.spawn_with_handle());

        // only allow outgoing messages while we are successfully reading incoming ones
        self.tasks.push(
            async move {
                let mut actor = send_to_socket::Actor::new(write, noise.clone());

                let _heartbeat_handle = out_msg_actor_context
                    .notify_interval(HEARTBEAT_INTERVAL, || wire::MakerToTaker::Heartbeat)
                    .expect("actor not to shutdown")
                    .spawn_with_handle();

                out_msg_actor_context
                    .handle_while(&mut actor, forward_to_cfd.attach_stream(read))
                    .await;

                tracing::error!("Closing connection to taker {}", taker_id);

                actor.shutdown().await;
            }
            .spawn_with_handle(),
        );

        self.write_connections
            .insert(taker_id, out_msg_actor_address);

        let _ = self
            .new_taker_channel
            .send(maker_cfd::NewTakerOnline { id: taker_id })
            .await;

        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("No connection to taker {0}")]
pub struct NoConnection(TakerId);

#[xtra_productivity]
impl Actor {
    async fn handle_broadcast_order(&mut self, msg: BroadcastOrder) {
        let order = msg.0;
        for taker_id in self.write_connections.clone().keys() {
            self.send_to_taker(taker_id, wire::MakerToTaker::CurrentOrder(order.clone())).await.expect("send_to_taker only fails on missing hashmap entry and we are iterating over those entries");
            tracing::trace!(%taker_id, "sent new order: {:?}", order.as_ref().map(|o| o.id));
        }
    }

    async fn handle_taker_message(&mut self, msg: TakerMessage) -> Result<(), NoConnection> {
        match msg.command {
            TakerCommand::SendOrder { order } => {
                self.send_to_taker(&msg.taker_id, wire::MakerToTaker::CurrentOrder(order))
                    .await?;
            }
            TakerCommand::NotifyInvalidOrderId { id } => {
                self.send_to_taker(&msg.taker_id, wire::MakerToTaker::InvalidOrderId(id))
                    .await?;
            }
            TakerCommand::NotifyOrderAccepted { id } => {
                self.send_to_taker(&msg.taker_id, wire::MakerToTaker::ConfirmOrder(id))
                    .await?;
            }
            TakerCommand::NotifyOrderRejected { id } => {
                self.send_to_taker(&msg.taker_id, wire::MakerToTaker::RejectOrder(id))
                    .await?;
            }
            TakerCommand::NotifySettlementAccepted { id } => {
                self.send_to_taker(&msg.taker_id, wire::MakerToTaker::ConfirmSettlement(id))
                    .await?;
            }
            TakerCommand::NotifySettlementRejected { id } => {
                self.send_to_taker(&msg.taker_id, wire::MakerToTaker::RejectSettlement(id))
                    .await?;
            }
            TakerCommand::Protocol(setup_msg) => {
                self.send_to_taker(&msg.taker_id, wire::MakerToTaker::Protocol(setup_msg))
                    .await?;
            }
            TakerCommand::NotifyRollOverAccepted {
                id,
                oracle_event_id,
            } => {
                self.send_to_taker(
                    &msg.taker_id,
                    wire::MakerToTaker::ConfirmRollOver {
                        order_id: id,
                        oracle_event_id,
                    },
                )
                .await?;
            }
            TakerCommand::NotifyRollOverRejected { id } => {
                self.send_to_taker(&msg.taker_id, wire::MakerToTaker::RejectRollOver(id))
                    .await?;
            }
            TakerCommand::RollOverProtocol(roll_over_msg) => {
                self.send_to_taker(
                    &msg.taker_id,
                    wire::MakerToTaker::RollOverProtocol(roll_over_msg),
                )
                .await?;
            }
        }

        Ok(())
    }

    async fn handle(&mut self, msg: ListenerMessage, ctx: &mut Context<Self>) -> KeepRunning {
        match msg {
            ListenerMessage::NewConnection { stream, address } => {
                if let Err(err) = self.handle_new_connection_impl(stream, address, ctx).await {
                    tracing::warn!("Maker was unable to negotiate a new connection: {}", err);
                }
                KeepRunning::Yes
            }
            ListenerMessage::Error { source } => {
                tracing::warn!("TCP listener produced an error: {}", source);

                // Maybe we should move the actual listening on the socket into here and restart the
                // actor upon an error?
                KeepRunning::Yes
            }
        }
    }
}

impl xtra::Actor for Actor {}
