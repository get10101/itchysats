use crate::maker_cfd::{FromTaker, TakerConnected, TakerDisconnected};
use crate::model::cfd::Order;
use crate::model::Identity;
use crate::noise::TransportStateExt;
use crate::tokio_ext::FutureExt;
use crate::{forward_only_ok, maker_cfd, noise, send_to_socket, wire, Tasks};
use anyhow::Result;
use futures::{StreamExt, TryStreamExt};
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_util::codec::FramedRead;
use xtra::prelude::*;
use xtra::{Actor as _, KeepRunning};
use xtra_productivity::xtra_productivity;

pub struct BroadcastOrder(pub Option<Order>);

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
    tasks: Tasks,
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
            tasks: Tasks::default(),
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
            tracing::info!(%taker_id, "Failed to send {} to taker, removing connection", msg_str);
            if self.write_connections.remove(taker_id).is_some() {
                let _ = self
                    .taker_disconnected_channel
                    .send(maker_cfd::TakerDisconnected { id: *taker_id })
                    .await;
            }
        }

        Ok(())
    }

    async fn handle_new_connection_impl(
        &mut self,
        mut stream: TcpStream,
        taker_address: SocketAddr,
        _: &mut Context<Self>,
    ) -> Result<()> {
        let transport_state = noise::responder_handshake(&mut stream, &self.noise_priv_key).await?;
        let taker_id = Identity::new(transport_state.get_remote_public_key()?);

        tracing::info!(%taker_id, address = %taker_address, "New taker connected");

        let transport_state = Arc::new(Mutex::new(transport_state));

        let (read, write) = stream.into_split();
        let read = FramedRead::new(read, wire::EncryptedJsonCodec::new(transport_state.clone()))
            .map_ok(move |msg| FromTaker { taker_id, msg })
            .map(forward_only_ok::Message);

        let (out_msg_actor_address, mut out_msg_actor_context) = xtra::Context::new(None);

        let (forward_to_cfd, forward_to_cfd_fut) =
            forward_only_ok::Actor::new(self.taker_msg_channel.clone_channel())
                .create(None)
                .run();
        self.tasks.add(forward_to_cfd_fut);

        // only allow outgoing messages while we are successfully reading incoming ones
        let heartbeat_interval = self.heartbeat_interval;
        let taker_disconnected_channel = self.taker_disconnected_channel.clone_channel();
        self.tasks.add(async move {
            let mut actor = send_to_socket::Actor::new(write, transport_state.clone());

            let _heartbeat_handle = out_msg_actor_context
                .notify_interval(heartbeat_interval, || wire::MakerToTaker::Heartbeat)
                .expect("actor not to shutdown")
                .spawn_with_handle();

            out_msg_actor_context
                .handle_while(&mut actor, forward_to_cfd.attach_stream(read))
                .await;

            tracing::error!("Closing connection to taker {}", taker_id);
            let _ = taker_disconnected_channel
                .send(maker_cfd::TakerDisconnected { id: taker_id })
                .await;

            actor.shutdown().await;
        });

        self.write_connections
            .insert(taker_id, out_msg_actor_address);

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
}

impl xtra::Actor for Actor {}
