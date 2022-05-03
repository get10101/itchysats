use crate::collab_settlement_maker;
use crate::future_ext::FutureExt;
use crate::maker_cfd;
use crate::noise;
use crate::noise::TransportStateExt;
use crate::rollover_maker;
use crate::setup_maker;
use crate::wire;
use crate::wire::taker_to_maker;
use crate::wire::EncryptedJsonCodec;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use futures::SinkExt;
use futures::StreamExt;
use futures::TryStreamExt;
use model::Identity;
use model::MakerOffers;
use model::OrderId;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio_tasks::Tasks;
use tokio_util::codec::Framed;
use xtra::message_channel::MessageChannel;
use xtra_productivity::xtra_productivity;
use xtras::address_map::NotConnected;
use xtras::AddressMap;
use xtras::SendAsyncSafe;
use xtras::SendInterval;

#[derive(Clone, Copy)]
pub struct BroadcastOffers(pub Option<MakerOffers>);

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

pub mod settlement {
    use super::*;

    /// Message sent from the `collab_settlement_maker::Actor` to the
    /// `maker_inc_connections::Actor` so that it can forward it to the
    /// taker.
    ///
    /// Additionally, the address of this instance of the
    /// `collab_settlement_maker::Actor` is included so that the
    /// `maker_inc_connections::Actor` knows where to forward the
    /// collaborative settlement messages from the taker about this
    /// particular order.
    pub struct Response {
        pub taker_id: Identity,
        pub order_id: OrderId,
        pub decision: Decision,
    }

    pub enum Decision {
        Accept {
            address: xtra::Address<collab_settlement_maker::Actor>,
        },
        Reject,
    }
}

pub struct TakerMessage {
    pub taker_id: Identity,
    pub msg: wire::MakerToTaker,
}

pub struct RegisterRollover {
    pub order_id: OrderId,
    pub address: xtra::Address<rollover_maker::Actor>,
}

pub struct Actor {
    connections: HashMap<Identity, Connection>,
    taker_connected_channel: Box<dyn MessageChannel<maker_cfd::TakerConnected>>,
    taker_disconnected_channel: Box<dyn MessageChannel<maker_cfd::TakerDisconnected>>,
    taker_msg_channel: Box<dyn MessageChannel<maker_cfd::FromTaker>>,
    noise_priv_key: x25519_dalek::StaticSecret,
    heartbeat_interval: Duration,
    p2p_socket: SocketAddr,
    setup_actors: AddressMap<OrderId, setup_maker::Actor>,
    settlement_actors: AddressMap<OrderId, collab_settlement_maker::Actor>,
    rollover_actors: AddressMap<OrderId, rollover_maker::Actor>,
    tasks: Tasks,
}

/// A connection to a taker.
struct Connection {
    taker: Identity,
    write: wire::Write<wire::TakerToMaker, wire::MakerToTaker>,
    wire_version: wire::Version,
    daemon_version: String,
    _tasks: Tasks,
}

impl Connection {
    async fn send(&mut self, msg: wire::MakerToTaker) -> Result<()> {
        let msg_str = msg.name();
        let taker_id = self.taker;

        P2P_MESSAGES_SENT
            .with(&HashMap::from([(MESSAGE_LABEL, msg_str)]))
            .inc();

        tracing::trace!(target: "wire", %taker_id, msg_name = msg_str, "Sending");

        let taker_version = self.wire_version.clone();

        // Transform messages based on version compatibility
        let msg = if taker_version == wire::Version::LATEST {
            // Connection is using the latest version, no transformation needed
            msg
        } else if taker_version == wire::Version::V2_0_0 {
            // Connection is for version `2.0.0`. Be backwards compatible by sending `CurrentOrder`
            // instead of `CurrentOffer`
            match msg {
                wire::MakerToTaker::CurrentOffers(offers) => {
                    wire::MakerToTaker::CurrentOrder(offers.and_then(|offers| offers.short).map(
                        |order| wire::DeprecatedOrder047 {
                            id: order.id,
                            trading_pair: order.trading_pair,
                            position_maker: order.position_maker,
                            price: order.price,
                            min_quantity: order.min_quantity,
                            max_quantity: order.max_quantity,
                            leverage_taker: order.leverage_taker,
                            creation_timestamp: order.creation_timestamp_maker,
                            settlement_interval: order.settlement_interval,
                            liquidation_price: model::calculate_long_liquidation_price(
                                order.leverage_taker,
                                order.price,
                            ),
                            origin: order.origin,
                            oracle_event_id: order.oracle_event_id,
                            tx_fee_rate: order.tx_fee_rate,
                            funding_rate: order.funding_rate,
                            opening_fee: order.opening_fee,
                        },
                    ))
                }
                _ => msg,
            }
        } else {
            bail!("Don't know how to send {msg_str} to taker with version {taker_version}");
        };

        self.write
            .send(msg)
            .await
            .with_context(|| format!("Failed to send msg {msg_str} to taker {taker_id}"))?;

        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let taker_id = self.taker;

        tracing::debug!(%taker_id, "Connection got dropped");
    }
}

impl Actor {
    pub fn new(
        taker_connected_channel: Box<dyn MessageChannel<maker_cfd::TakerConnected>>,
        taker_disconnected_channel: Box<dyn MessageChannel<maker_cfd::TakerDisconnected>>,
        taker_msg_channel: Box<dyn MessageChannel<maker_cfd::FromTaker>>,
        noise_priv_key: x25519_dalek::StaticSecret,
        heartbeat_interval: Duration,
        p2p_socket: SocketAddr,
    ) -> Self {
        NUM_CONNECTIONS_GAUGE.reset();

        Self {
            connections: HashMap::new(),
            taker_connected_channel: taker_connected_channel.clone_channel(),
            taker_disconnected_channel: taker_disconnected_channel.clone_channel(),
            taker_msg_channel: taker_msg_channel.clone_channel(),
            noise_priv_key,
            heartbeat_interval,
            p2p_socket,
            setup_actors: AddressMap::default(),
            settlement_actors: AddressMap::default(),
            rollover_actors: AddressMap::default(),
            tasks: Tasks::default(),
        }
    }

    async fn drop_taker_connection(&mut self, taker_id: &Identity) {
        if let Some(connection) = self.connections.remove(taker_id) {
            let _: Result<(), xtra::Disconnected> = self
                .taker_disconnected_channel
                .send_async_safe(maker_cfd::TakerDisconnected { id: *taker_id })
                .await;

            NUM_CONNECTIONS_GAUGE
                .with(&HashMap::from([
                    (
                        WIRE_VERSION_LABEL,
                        connection.wire_version.to_string().as_str(),
                    ),
                    (DAEMON_VERSION_LABEL, connection.daemon_version.as_str()),
                ]))
                .dec();
        } else {
            tracing::debug!(%taker_id, "No active connection to taker");

            // TODO: Re-compute metrics here by iteration of all connections? If this happens often
            // we might skew our metrics.
        }
    }

    async fn send_to_taker(
        &mut self,
        taker_id: &Identity,
        msg: wire::MakerToTaker,
    ) -> Result<(), NoConnection> {
        let conn = self
            .connections
            .get_mut(taker_id)
            .ok_or(NoConnection(*taker_id))?;

        if conn.send(msg).await.is_err() {
            self.drop_taker_connection(taker_id).await;
            return Err(NoConnection(*taker_id));
        }

        Ok(())
    }

    async fn start_listener(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");
        let address = self.p2p_socket;

        let listener = match TcpListener::bind(address)
            .await
            .with_context(|| format!("Failed to bind to socket {address}"))
        {
            Ok(listener) => listener,
            Err(error) => {
                let _ = this.send_async_safe(ListenerFailed { error }).await;
                return;
            }
        };

        let local_address = listener
            .local_addr()
            .expect("listener to have local address");

        tracing::info!("Listening on {local_address}");

        let noise_priv_key = self.noise_priv_key.clone();

        self.tasks.add(async move {
            let mut tasks = Tasks::default();

            loop {
                let new_connection = listener
                    .accept()
                    .await
                    .context("Failed to accept new connection");

                match new_connection {
                    Ok((stream, address)) => {
                        let upgrade = upgrade(stream, noise_priv_key.clone(), this.clone());

                        tasks
                            .add_fallible(
                                upgrade,
                                move |e| async move {
                                    tracing::warn!(address = %address, "Failed to upgrade incoming connection: {:#}", e);
                                }
                            );
                    }
                    Err(error) => {
                        let _ = this.send(ListenerFailed { error }).await;
                        return;
                    }
                }
            }
        });
    }
}

struct SendHeartbeat(Identity);

#[derive(Debug, thiserror::Error, Clone, Copy)]
#[error("No connection to taker {0}")]
pub struct NoConnection(Identity);

#[xtra_productivity]
impl Actor {
    async fn handle_broadcast_order(&mut self, msg: BroadcastOffers) {
        let offers = msg.0;

        let mut broken_connections = Vec::with_capacity(self.connections.len());

        for (id, conn) in &mut self.connections {
            if let Err(e) = conn.send(wire::MakerToTaker::CurrentOffers(offers)).await {
                tracing::warn!("{:#}", e);
                broken_connections.push(*id);

                continue;
            }

            tracing::trace!(taker_id = %id, "Sent new offers: {:?}", offers);
        }

        for id in broken_connections {
            self.drop_taker_connection(&id).await;
        }
    }

    async fn handle_send_heartbeat(&mut self, msg: SendHeartbeat) {
        match self
            .send_to_taker(&msg.0, wire::MakerToTaker::Heartbeat)
            .await
        {
            Ok(()) => {}
            Err(NoConnection(taker_id)) => {
                tracing::trace!(%taker_id, "Failed to send heartbeat because connection is gone");
            }
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

    async fn handle_settlement_response(&mut self, msg: settlement::Response) -> Result<()> {
        let decision = match msg.decision {
            settlement::Decision::Accept { address } => {
                self.settlement_actors.insert(msg.order_id, address);

                wire::maker_to_taker::Settlement::Confirm
            }
            settlement::Decision::Reject => wire::maker_to_taker::Settlement::Reject,
        };

        self.send_to_taker(
            &msg.taker_id,
            wire::MakerToTaker::Settlement {
                order_id: msg.order_id,
                msg: decision,
            },
        )
        .await?;

        Ok(())
    }

    async fn handle_taker_message(&mut self, msg: TakerMessage) -> Result<(), NoConnection> {
        self.send_to_taker(&msg.taker_id, msg.msg).await?;

        Ok(())
    }

    async fn handle_read_fail(&mut self, msg: ReadFail) {
        let ReadFail { taker_id, error } = msg;

        tracing::debug!(%taker_id, "Failed to read incoming messages from taker: {error:#}");

        self.drop_taker_connection(&taker_id).await;
    }

    async fn handle_connection_ready(
        &mut self,
        ConnectionReady {
            mut read,
            write,
            identity,
            address,
            wire_version,
            daemon_version,
        }: ConnectionReady,
        ctx: &mut xtra::Context<Self>,
    ) {
        let this = ctx.address().expect("we are alive");

        if self.connections.contains_key(&identity) {
            tracing::debug!(
                taker_id = %identity,
                "Refusing to accept 2nd connection from already connected taker!"
            );
            return;
        }

        let _: Result<(), xtra::Disconnected> = self
            .taker_connected_channel
            .send_async_safe(maker_cfd::TakerConnected { id: identity })
            .await;

        let mut tasks = Tasks::default();
        tasks.add_fallible(
            {
                let this = this.clone();

                async move {
                    loop {
                        let msg = read
                            .try_next()
                            .await
                            .context("Failed to read from socket")?
                            .context("End of stream")?;

                        this.send(maker_cfd::FromTaker {
                            taker_id: identity,
                            msg,
                        })
                        .await
                        .context(
                            "we are not connected to ourselves, this should really not happen",
                        )?;
                    }
                }
            },
            {
                let this = this.clone();

                move |error| async move {
                    let _ = this
                        .send(ReadFail {
                            taker_id: identity,
                            error,
                        })
                        .await;
                }
            },
        );
        tasks.add(this.send_interval(self.heartbeat_interval, move || SendHeartbeat(identity)));

        self.connections.insert(
            identity,
            Connection {
                taker: identity,
                write,
                wire_version: wire_version.clone(),
                daemon_version: daemon_version.clone(),
                _tasks: tasks,
            },
        );

        NUM_CONNECTIONS_GAUGE
            .with(&HashMap::from([
                (WIRE_VERSION_LABEL, wire_version.to_string().as_str()),
                (DAEMON_VERSION_LABEL, daemon_version.as_str()),
            ]))
            .inc();

        tracing::debug!(taker_id = %identity, taker_addres = %address, %wire_version, %daemon_version, "Connection is ready");
    }

    async fn handle_listener_failed(&mut self, msg: ListenerFailed, ctx: &mut xtra::Context<Self>) {
        tracing::warn!("TCP listener failed: {:#}", msg.error);
        ctx.stop();
    }

    async fn handle_rollover_proposed(&mut self, msg: RegisterRollover) {
        self.rollover_actors.insert(msg.order_id, msg.address);
    }
}

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle_msg_from_taker(&mut self, msg: maker_cfd::FromTaker) {
        let msg_str = msg.msg.name();

        tracing::trace!(target: "wire", taker_id = %msg.taker_id, msg_name = msg_str, "Received");

        use wire::TakerToMaker::*;
        match msg.msg {
            Protocol { order_id, msg } => {
                if let Err(NotConnected(_)) = self.setup_actors.send_async(&order_id, msg).await {
                    tracing::warn!(%order_id, "No active setup actor");
                }
            }
            RolloverProtocol { order_id, msg } => {
                if let Err(NotConnected(_)) = self
                    .rollover_actors
                    .send_async(&order_id, rollover_maker::ProtocolMsg(msg))
                    .await
                {
                    tracing::warn!(%order_id, "No active rollover actor");
                }
            }
            Settlement {
                order_id,
                msg: taker_to_maker::Settlement::Initiate { sig_taker },
            } => {
                if let Err(NotConnected(_)) = self
                    .settlement_actors
                    .send_async(&order_id, collab_settlement_maker::Initiated { sig_taker })
                    .await
                {
                    tracing::warn!(%order_id, "No active settlement actor");
                }
            }
            TakeOrder { .. }
            | ProposeRollover { .. }
            | ProposeRolloverV2 { .. }
            | Settlement {
                order_id: _,
                msg: taker_to_maker::Settlement::Propose { .. },
            } => {
                // dispatch to the maker cfd actor
                let _ = self.taker_msg_channel.send_async_safe(msg).await;
            }
            Hello(_) | HelloV2 { .. } => {
                if cfg!(debug_assertions) {
                    unreachable!("Message {} is not dispatched to this actor", msg.msg.name())
                }
            }
            Unknown => {
                // Ignore unknown messages to be forwards-compatible.
            }
        }
    }
}

/// Upgrades a TCP stream to an encrypted transport, checking the network version in the process.
///
/// Both IO operations, upgrading to noise and checking the version are gated by a timeout.
async fn upgrade(
    mut stream: TcpStream,
    noise_priv_key: x25519_dalek::StaticSecret,
    this: xtra::Address<Actor>,
) -> Result<()> {
    let taker_address = stream.peer_addr().context("Failed to get peer address")?;

    tracing::debug!(%taker_address, "Upgrade new connection");

    let transport_state = noise::responder_handshake(&mut stream, &noise_priv_key)
        .timeout(Duration::from_secs(20))
        .await
        .context("Failed to complete noise handshake within 20 seconds")??;
    let taker_id = Identity::new(transport_state.get_remote_public_key()?);

    let (mut write, mut read) =
        Framed::new(stream, EncryptedJsonCodec::new(transport_state)).split();

    let first_message = read
        .try_next()
        .timeout(Duration::from_secs(10))
        .await
        .context("No message from taker within 10 seconds")?
        .context("Failed to read first message on stream")?
        .context("Stream closed before first message")?;

    let (proposed_wire_version, daemon_version) = match first_message {
        wire::TakerToMaker::Hello(proposed_wire_version) => (proposed_wire_version, None),
        wire::TakerToMaker::HelloV2 {
            proposed_wire_version,
            daemon_version,
        } => (proposed_wire_version, Some(daemon_version)),
        unexpected_message => {
            bail!(
                "Unexpected message {} from taker {taker_id}",
                unexpected_message.name()
            );
        }
    };

    let negotiated_wire_version = if proposed_wire_version == wire::Version::LATEST {
        wire::Version::LATEST
    } else if proposed_wire_version == wire::Version::V2_0_0 {
        wire::Version::V2_0_0
    } else {
        let our_version = wire::Version::LATEST; // If taker is incompatible, we tell them the latest version

        // write early here so we can bail afterwards
        write
            .send(wire::MakerToTaker::Hello(our_version.clone()))
            .await?;

        bail!("Network version negotiation failed, taker proposed {proposed_wire_version} but are on {our_version}");
    };

    write
        .send(wire::MakerToTaker::Hello(negotiated_wire_version.clone()))
        .await?;

    let daemon_version = daemon_version.unwrap_or_else(|| String::from("<= 0.4.7"));

    tracing::trace!(%taker_id, %taker_address, %negotiated_wire_version, %daemon_version, "Connection upgrade successful");

    let _ = this
        .send(ConnectionReady {
            read,
            write,
            identity: taker_id,
            address: taker_address,
            wire_version: negotiated_wire_version,
            daemon_version,
        })
        .await;

    Ok(())
}

struct ConnectionReady {
    read: wire::Read<wire::TakerToMaker, wire::MakerToTaker>,
    write: wire::Write<wire::TakerToMaker, wire::MakerToTaker>,
    identity: Identity,
    address: SocketAddr,
    wire_version: wire::Version,
    daemon_version: String,
}

struct ReadFail {
    taker_id: Identity,
    error: anyhow::Error,
}

struct ListenerFailed {
    error: anyhow::Error,
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        self.start_listener(ctx).await;
    }

    async fn stopped(self) -> Self::Stop {}
}

const WIRE_VERSION_LABEL: &str = "wire_version";
const DAEMON_VERSION_LABEL: &str = "daemon_version";

static NUM_CONNECTIONS_GAUGE: conquer_once::Lazy<prometheus::IntGaugeVec> =
    conquer_once::Lazy::new(|| {
        prometheus::register_int_gauge_vec!(
            "p2p_connections_total",
            "The number of active p2p connections.",
            &[WIRE_VERSION_LABEL, DAEMON_VERSION_LABEL]
        )
        .unwrap()
    });

const MESSAGE_LABEL: &str = "message";

static P2P_MESSAGES_SENT: conquer_once::Lazy<prometheus::IntCounterVec> =
    conquer_once::Lazy::new(|| {
        prometheus::register_int_counter_vec!(
            "p2p_messages_sent_total",
            "The number of messages sent over the p2p connection.",
            &[MESSAGE_LABEL]
        )
        .unwrap()
    });
