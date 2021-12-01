use crate::address_map::{AddressMap, Stopping};
use crate::model::cfd::OrderId;
use crate::model::{Price, Timestamp, Usd};
use crate::tokio_ext::FutureExt;
use crate::{collab_settlement_taker, log_error, noise, send_to_socket, setup_taker, wire, Tasks};
use anyhow::{Context, Result};
use bdk::bitcoin::Amount;
use futures::StreamExt;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_util::codec::FramedRead;
use xtra::prelude::MessageChannel;
use xtra::KeepRunning;
use xtra_productivity::xtra_productivity;

/// Time between reconnection attempts
const CONNECT_TO_MAKER_INTERVAL: Duration = Duration::from_secs(5);

struct ConnectedState {
    last_heartbeat: SystemTime,
    _tasks: Tasks,
}

pub struct Actor {
    status_sender: watch::Sender<ConnectionStatus>,
    send_to_maker: Box<dyn MessageChannel<wire::TakerToMaker>>,
    send_to_maker_ctx: xtra::Context<send_to_socket::Actor<wire::TakerToMaker>>,
    identity_sk: x25519_dalek::StaticSecret,
    maker_to_taker: Box<dyn MessageChannel<wire::MakerToTaker>>,
    /// Max duration since the last heartbeat until we die.
    heartbeat_timeout: Duration,
    connect_timeout: Duration,
    connected_state: Option<ConnectedState>,
    setup_actors: HashMap<OrderId, xtra::Address<setup_taker::Actor>>,
    collab_settlement_actors: AddressMap<OrderId, collab_settlement_taker::Actor>,
}

pub struct Connect {
    pub maker_identity_pk: x25519_dalek::PublicKey,
    pub maker_addr: SocketAddr,
}

pub struct MakerStreamMessage {
    pub item: Result<wire::MakerToTaker>,
}

/// Private message to measure the current pulse (i.e. check when we received the last heartbeat).
struct MeasurePulse;

#[derive(Clone, Debug, PartialEq)]
pub enum ConnectionStatus {
    Online,
    Offline,
}

/// Message sent from the `setup_taker::Actor` to the
/// `connection::Actor` so that it can forward it to the maker.
///
/// Additionally, the address of this instance of the
/// `setup_taker::Actor` is included so that the `connection::Actor`
/// knows where to forward the contract setup messages from the maker
/// about this particular order.
pub struct TakeOrder {
    pub order_id: OrderId,
    pub quantity: Usd,
    pub address: xtra::Address<setup_taker::Actor>,
}

pub struct ProposeSettlement {
    pub order_id: OrderId,
    pub timestamp: Timestamp,
    pub taker: Amount,
    pub maker: Amount,
    pub price: Price,
    pub address: xtra::Address<collab_settlement_taker::Actor>,
}

impl Actor {
    pub fn new(
        status_sender: watch::Sender<ConnectionStatus>,
        maker_to_taker: Box<dyn MessageChannel<wire::MakerToTaker>>,
        identity_sk: x25519_dalek::StaticSecret,
        hearthbeat_timeout: Duration,
        connect_timeout: Duration,
    ) -> Self {
        let (send_to_maker_addr, send_to_maker_ctx) = xtra::Context::new(None);

        Self {
            status_sender,
            send_to_maker: Box::new(send_to_maker_addr),
            send_to_maker_ctx,
            identity_sk,
            maker_to_taker,
            heartbeat_timeout: hearthbeat_timeout,
            connected_state: None,
            setup_actors: HashMap::new(),
            connect_timeout,
            collab_settlement_actors: AddressMap::default(),
        }
    }
}

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle_taker_to_maker(&mut self, message: wire::TakerToMaker) {
        log_error!(self.send_to_maker.send(message));
    }

    async fn handle_collab_settlement_actor_stopping(
        &mut self,
        message: Stopping<collab_settlement_taker::Actor>,
    ) {
        self.collab_settlement_actors.gc(message);
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle_take_order(&mut self, msg: TakeOrder) -> Result<()> {
        self.send_to_maker
            .send(wire::TakerToMaker::TakeOrder {
                order_id: msg.order_id,
                quantity: msg.quantity,
            })
            .await?;

        self.setup_actors.insert(msg.order_id, msg.address);

        Ok(())
    }

    async fn handle_propose_settlement(&mut self, msg: ProposeSettlement) -> Result<()> {
        let ProposeSettlement {
            order_id,
            timestamp,
            taker,
            maker,
            price,
            address,
        } = msg;

        self.send_to_maker
            .send(wire::TakerToMaker::Settlement {
                order_id,
                msg: wire::taker_to_maker::Settlement::Propose {
                    timestamp,
                    taker,
                    maker,
                    price,
                },
            })
            .await?;

        self.collab_settlement_actors.insert(order_id, address);

        Ok(())
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle_connect(
        &mut self,
        Connect {
            maker_addr,
            maker_identity_pk,
        }: Connect,
        ctx: &mut xtra::Context<Self>,
    ) -> Result<()> {
        tracing::debug!(address = %maker_addr, "Connecting to maker");

        let (read, write, noise) = {
            let mut connection = TcpStream::connect(&maker_addr)
                .timeout(self.connect_timeout)
                .await
                .with_context(|| {
                    format!(
                        "Connection attempt to {} timed out after {}s",
                        maker_addr,
                        self.connect_timeout.as_secs()
                    )
                })?
                .with_context(|| format!("Failed to connect to {}", maker_addr))?;
            let noise =
                noise::initiator_handshake(&mut connection, &self.identity_sk, &maker_identity_pk)
                    .await?;

            let (read, write) = connection.into_split();
            (read, write, Arc::new(Mutex::new(noise)))
        };

        let send_to_socket = send_to_socket::Actor::new(write, noise.clone());

        let mut tasks = Tasks::default();
        tasks.add(self.send_to_maker_ctx.attach(send_to_socket));

        let read = FramedRead::new(read, wire::EncryptedJsonCodec::new(noise))
            .map(move |item| MakerStreamMessage { item });

        let this = ctx.address().expect("self to be alive");
        tasks.add(this.attach_stream(read));

        tasks.add(
            ctx.notify_interval(self.heartbeat_timeout, || MeasurePulse)
                .expect("we just started"),
        );

        self.connected_state = Some(ConnectedState {
            last_heartbeat: SystemTime::now(),
            _tasks: tasks,
        });
        self.status_sender
            .send(ConnectionStatus::Online)
            .expect("receiver to outlive the actor");

        tracing::info!(address = %maker_addr, "Established connection to maker");

        Ok(())
    }

    async fn handle_wire_message(
        &mut self,
        message: MakerStreamMessage,
        _ctx: &mut xtra::Context<Self>,
    ) -> KeepRunning {
        let msg = match message.item {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!("Error while receiving message from maker: {:#}", e);
                return KeepRunning::Yes;
            }
        };

        tracing::trace!("Received '{}'", msg);

        match msg {
            wire::MakerToTaker::Heartbeat => {
                self.connected_state
                    .as_mut()
                    .expect("wire messages only to arrive in connected state")
                    .last_heartbeat = SystemTime::now();
            }
            wire::MakerToTaker::ConfirmOrder(order_id) => match self.setup_actors.get(&order_id) {
                Some(addr) => {
                    let _ = addr.send(setup_taker::Accepted).await;
                }
                None => {
                    tracing::warn!(%order_id, "No active contract setup");
                }
            },
            wire::MakerToTaker::RejectOrder(order_id) => match self.setup_actors.get(&order_id) {
                Some(addr) => {
                    let _ = addr.send(setup_taker::Rejected::without_reason()).await;
                }
                None => {
                    tracing::warn!(%order_id, "No active contract setup");
                }
            },
            wire::MakerToTaker::Protocol { order_id, msg } => {
                match self.setup_actors.get(&order_id) {
                    Some(addr) => {
                        let _ = addr.send(msg).await;
                    }
                    None => {
                        tracing::warn!(%order_id, "No active contract setup");
                    }
                }
            }
            wire::MakerToTaker::InvalidOrderId(order_id) => {
                match self.setup_actors.get(&order_id) {
                    Some(addr) => {
                        let _ = addr.send(setup_taker::Rejected::invalid_order_id()).await;
                    }
                    None => {
                        tracing::warn!(%order_id, "No active contract setup");
                    }
                }
            }
            wire::MakerToTaker::Settlement { order_id, msg } => {
                if !self.collab_settlement_actors.send(&order_id, msg).await {
                    tracing::warn!(%order_id, "No active collaborative settlement");
                }
            }
            other => {
                // this one should go to the taker cfd actor
                log_error!(self.maker_to_taker.send(other));
            }
        }
        KeepRunning::Yes
    }

    fn handle_measure_pulse(&mut self, _: MeasurePulse) {
        let time_since_last_heartbeat = SystemTime::now()
            .duration_since(
                self.connected_state
                    .as_ref()
                    .expect("only run pulse measurements if connected")
                    .last_heartbeat,
            )
            .expect("now is always later than heartbeat");

        if time_since_last_heartbeat > self.heartbeat_timeout {
            self.status_sender
                .send(ConnectionStatus::Offline)
                .expect("watch receiver to outlive the actor");
            self.connected_state = None;
        }
    }
}

impl xtra::Actor for Actor {}

// TODO: Move the reconnection logic inside the connection::Actor instead of
// depending on a watch channel
pub async fn connect(
    mut maker_online_status_feed_receiver: watch::Receiver<ConnectionStatus>,
    connection_actor_addr: xtra::Address<Actor>,
    maker_identity_pk: x25519_dalek::PublicKey,
    maker_addresses: Vec<SocketAddr>,
) {
    loop {
        if maker_online_status_feed_receiver.borrow().clone() == ConnectionStatus::Offline {
            tracing::debug!("No connection to the maker");
            'connect: loop {
                for address in &maker_addresses {
                    let connect_msg = Connect {
                        maker_identity_pk,
                        maker_addr: *address,
                    };

                    if let Err(e) = connection_actor_addr
                        .send(connect_msg)
                        .await
                        .expect("Taker actor to be present")
                    {
                        tracing::trace!(%address, "Failed to establish connection: {:#}", e);
                        continue;
                    }
                    break 'connect;
                }

                tracing::warn!(
                    "Tried connecting to {} addresses without success, retrying in {} seconds",
                    maker_addresses.len(),
                    CONNECT_TO_MAKER_INTERVAL.as_secs()
                );

                tokio::time::sleep(CONNECT_TO_MAKER_INTERVAL).await;
            }
        }
        maker_online_status_feed_receiver
            .changed()
            .await
            .expect("watch channel should outlive the future");
    }
}
