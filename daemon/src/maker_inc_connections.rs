use crate::address_map::{AddressMap, Stopping};
use crate::maker_cfd::{FromTaker, TakerConnected, TakerDisconnected};
use crate::model::cfd::{Order, OrderId};
use crate::model::Identity;
use crate::noise::TransportStateExt;
use crate::tokio_ext::FutureExt;
use crate::wire::{EncryptedJsonCodec, MakerToTaker, TakerToMaker, Version};
use crate::{maker_cfd, noise, send_to_socket, setup_maker, wire, Tasks};
use anyhow::{bail, Context, Result};
use futures::{SinkExt, TryStreamExt};
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_util::codec::{FramedRead, FramedWrite};
use xtra::prelude::*;
use xtra::KeepRunning;
use xtra_productivity::xtra_productivity;

pub struct BroadcastOrder(pub Option<Order>);

/// Message sent from the `setup_maker::Actor` to the
/// `maker_inc_connections::Actor` so that it can forward it to the
/// taker.
///
/// Additionally, the address of this instance of the
/// `setup_maker::Actor` is included so that the
/// `maker_inc_connections::Actor` knows where to forward the contract
/// setup messages from the taker about this particular order.
pub struct ConfirmOrder {
    pub taker_id: Identity,
    pub order_id: OrderId,
    pub address: xtra::Address<setup_maker::Actor>,
}

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
    setup_actors: AddressMap<OrderId, setup_maker::Actor>,
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
            setup_actors: AddressMap::default(),
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
        ctx: &mut xtra::Context<Self>,
    ) -> Result<()> {
        let transport_state = noise::responder_handshake(&mut stream, &self.noise_priv_key).await?;
        let taker_id = Identity::new(transport_state.get_remote_public_key()?);

        let transport_state = Arc::new(Mutex::new(transport_state));

        let (read, write) = stream.into_split();
        let mut read =
            FramedRead::new(read, wire::EncryptedJsonCodec::new(transport_state.clone()));
        let mut write = FramedWrite::new(write, EncryptedJsonCodec::new(transport_state));

        match read
            .try_next()
            .timeout(Duration::from_secs(10))
            .await
            .with_context(|| {
                format!(
                    "Taker {} did not send Hello within 10 seconds, dropping connection",
                    taker_id
                )
            })? {
            Ok(Some(TakerToMaker::Hello(taker_version))) => {
                let our_version = Version::current();
                write.send(MakerToTaker::Hello(our_version.clone())).await?;

                if our_version != taker_version {
                    tracing::debug!(
                        "Network version mismatch, we are on version {} but taker is on version {}",
                        our_version,
                        taker_version
                    );

                    // A taker running a different version is not treated as error for the maker
                    return Ok(());
                }
            }
            unexpected_message => {
                bail!(
                    "Unexpected message {:?} from taker {}",
                    unexpected_message,
                    taker_id
                );
            }
        }

        tracing::info!(%taker_id, address = %taker_address, "New taker connected");

        let this = ctx.address().expect("self to be alive");
        let read_fut = async move {
            while let Ok(Some(msg)) = read.try_next().await {
                let res = this.send(FromTaker { taker_id, msg }).await;

                if res.is_err() {
                    break;
                }
            }

            let _ = this.send(ReadFail(taker_id)).await;
        };

        let (out_msg, mut out_msg_actor_context) = xtra::Context::new(None);
        let send_to_socket_actor = send_to_socket::Actor::new(write);

        let heartbeat_fut = out_msg_actor_context
            .notify_interval(self.heartbeat_interval, || wire::MakerToTaker::Heartbeat)
            .expect("actor not to shutdown");

        let write_fut = out_msg_actor_context.run(send_to_socket_actor);

        self.write_connections.insert(taker_id, out_msg);

        let mut tasks = Tasks::default();
        tasks.add(read_fut, "read_fut");
        tasks.add(heartbeat_fut, "hearbeat_fut2");
        tasks.add(write_fut, "write_fut");
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

    async fn handle_confirm_order(&mut self, msg: ConfirmOrder) -> Result<()> {
        self.send_to_taker(
            &msg.taker_id,
            wire::MakerToTaker::ConfirmOrder(msg.order_id),
        )
        .await?;

        self.setup_actors.insert(msg.order_id, msg.address);

        Ok(())
    }

    async fn handle_taker_message(&mut self, msg: TakerMessage) -> Result<(), NoConnection> {
        self.send_to_taker(&msg.taker_id, msg.msg).await?;

        Ok(())
    }

    async fn handle(&mut self, msg: ListenerMessage, ctx: &mut xtra::Context<Self>) -> KeepRunning {
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

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle_msg_from_taker(&mut self, msg: FromTaker) {
        use wire::TakerToMaker::*;
        match msg.msg {
            Protocol { order_id, msg } => match self.setup_actors.get_connected(&order_id) {
                Some(addr) => {
                    let _ = addr.send(msg).await;
                }
                None => {
                    tracing::error!(%order_id, "No active contract setup");
                }
            },
            _ => {
                let _ = self.taker_msg_channel.send(msg);
            }
        }
    }

    async fn handle_setup_actor_stopping(&mut self, message: Stopping<setup_maker::Actor>) {
        self.setup_actors.gc(message);
    }
}

struct ReadFail(Identity);

impl xtra::Actor for Actor {}
