use crate::maker_cfd::{FromTaker, TakerConnected, TakerDisconnected};
use crate::model::cfd::Order;
use crate::model::Identity;
use crate::noise::TransportStateExt;
use crate::{maker_cfd, noise, send_to_socket, wire, Tasks};
use anyhow::Result;
use futures::TryStreamExt;
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_util::codec::FramedRead;
use xtra::prelude::*;
use xtra::KeepRunning;
use xtra_productivity::xtra_productivity;

pub struct BroadcastOrder(pub Option<Order>);

#[derive(Debug)]
pub struct TakerMessage {
    pub taker_id: Identity,
    pub msg: wire::MakerToTaker,
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
    write_connections: HashMap<Identity, Address<send_to_socket::Actor<wire::MakerToTaker>>>,
    taker_connected_channel: Box<dyn MessageChannel<TakerConnected>>,
    taker_disconnected_channel: Box<dyn MessageChannel<TakerDisconnected>>,
    taker_msg_channel: Box<dyn MessageChannel<FromTaker>>,
    noise_priv_key: x25519_dalek::StaticSecret,
    heartbeat_interval: Duration,
    connection_tasks: HashMap<Identity, Tasks>,
}

impl Actor {
    pub fn new(
        taker_connected_channel: Box<dyn MessageChannel<TakerConnected>>,
        taker_disconnected_channel: Box<dyn MessageChannel<TakerDisconnected>>,
        taker_msg_channel: Box<dyn MessageChannel<FromTaker>>,
        noise_priv_key: x25519_dalek::StaticSecret,
        heartbeat_interval: Duration,
    ) -> Self {
        Self {
            write_connections: HashMap::new(),
            taker_connected_channel: taker_connected_channel.clone_channel(),
            taker_disconnected_channel: taker_disconnected_channel.clone_channel(),
            taker_msg_channel: taker_msg_channel.clone_channel(),
            noise_priv_key,
            heartbeat_interval,
            connection_tasks: HashMap::new(),
        }
    }

    async fn drop_taker_connection(&mut self, taker_id: &Identity) {
        if self.write_connections.remove(taker_id).is_some() {
            tracing::info!(%taker_id, "Dropping connection");
            let _ = self
                .taker_disconnected_channel
                .send(maker_cfd::TakerDisconnected { id: *taker_id })
                .await;
            let _ = self.connection_tasks.remove(taker_id);
        }
    }

    async fn send_to_taker(
        &mut self,
        taker_id: &Identity,
        msg: wire::MakerToTaker,
    ) -> Result<(), NoConnection> {
        let conn = self
            .write_connections
            .get(taker_id)
            .ok_or_else(|| NoConnection(*taker_id))?;

        let msg_str = msg.to_string();

        if conn.send(msg).await.is_err() {
            tracing::error!(%taker_id, "Failed to send message to taker: {}", msg_str);
            self.drop_taker_connection(taker_id).await;
            return Err(NoConnection(*taker_id));
        }

        Ok(())
    }

    async fn handle_new_connection_impl(
        &mut self,
        mut stream: TcpStream,
        taker_address: SocketAddr,
        ctx: &mut Context<Self>,
    ) -> Result<()> {
        let transport_state = noise::responder_handshake(&mut stream, &self.noise_priv_key).await?;
        let taker_id = Identity::new(transport_state.get_remote_public_key()?);

        tracing::info!(%taker_id, address = %taker_address, "New taker connected");

        let transport_state = Arc::new(Mutex::new(transport_state));

        let (read, write) = stream.into_split();
        let mut read =
            FramedRead::new(read, wire::EncryptedJsonCodec::new(transport_state.clone()));

        let this = ctx.address().expect("self to be alive");
        let taker_msg_channel = self.taker_msg_channel.clone_channel();
        let read_fut = async move {
            while let Ok(Some(msg)) = read.try_next().await {
                let res = taker_msg_channel.send(FromTaker { taker_id, msg }).await;

                if res.is_err() {
                    break;
                }
            }

            let _ = this.send(ReadFail(taker_id)).await;
        };

        let (out_msg, mut out_msg_actor_context) = xtra::Context::new(None);
        let send_to_socket_actor = send_to_socket::Actor::new(write, transport_state.clone());

        let heartbeat_fut = out_msg_actor_context
            .notify_interval(self.heartbeat_interval, || wire::MakerToTaker::Heartbeat)
            .expect("actor not to shutdown");

        let write_fut = out_msg_actor_context.run(send_to_socket_actor);

        self.write_connections.insert(taker_id, out_msg);

        let mut tasks = Tasks::default();
        tasks.add(read_fut);
        tasks.add(heartbeat_fut);
        tasks.add(write_fut);
        self.connection_tasks.insert(taker_id, tasks);

        let _ = self
            .taker_connected_channel
            .send(maker_cfd::TakerConnected { id: taker_id })
            .await;

        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("No connection to taker {0}")]
pub struct NoConnection(Identity);

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
        self.send_to_taker(&msg.taker_id, msg.msg).await?;

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

    async fn handle_read_fail(&mut self, msg: ReadFail) {
        let taker_id = msg.0;
        tracing::error!(%taker_id, "Failed to read incoming messages from taker");

        self.drop_taker_connection(&taker_id).await;
    }
}

struct ReadFail(Identity);

impl xtra::Actor for Actor {}
