use crate::address_map::{AddressMap, Stopping};
use crate::model::cfd::OrderId;
use crate::model::{Identity, Price, Timestamp, Usd};
use crate::taker_cfd::CurrentOrder;
use crate::tokio_ext::FutureExt;
use crate::wire::{EncryptedJsonCodec, TakerToMaker, Version};
use crate::{
    collab_settlement_taker, log_error, noise, rollover_taker, send_to_socket, setup_taker, wire,
    Tasks,
};
use anyhow::{bail, Context, Result};
use bdk::bitcoin::Amount;
use futures::{SinkExt, StreamExt, TryStreamExt};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_util::codec::{FramedRead, FramedWrite};
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
    current_order: Box<dyn MessageChannel<CurrentOrder>>,
    /// Max duration since the last heartbeat until we die.
    heartbeat_timeout: Duration,
    connect_timeout: Duration,
    connected_state: Option<ConnectedState>,
    setup_actors: AddressMap<OrderId, setup_taker::Actor>,
    collab_settlement_actors: AddressMap<OrderId, collab_settlement_taker::Actor>,
    rollover_actors: AddressMap<OrderId, rollover_taker::Actor>,
}

pub struct Connect {
    pub maker_identity: Identity,
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
    Offline {
        reason: Option<ConnectionCloseReason>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConnectionCloseReason {
    VersionMismatch {
        taker_version: Version,
        maker_version: Version,
    },
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

pub struct ProposeRollOver {
    pub order_id: OrderId,
    pub timestamp: Timestamp,
    pub address: xtra::Address<rollover_taker::Actor>,
}

impl Actor {
    pub fn new(
        status_sender: watch::Sender<ConnectionStatus>,
        current_order: &(impl MessageChannel<CurrentOrder> + 'static),
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
            current_order: current_order.clone_channel(),
            heartbeat_timeout: hearthbeat_timeout,
            connected_state: None,
            setup_actors: AddressMap::default(),
            connect_timeout,
            collab_settlement_actors: AddressMap::default(),
            rollover_actors: AddressMap::default(),
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

    async fn handle_rollover_actor_stopping(&mut self, message: Stopping<rollover_taker::Actor>) {
        self.rollover_actors.gc(message);
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

    async fn handle_propose_roll_over(&mut self, msg: ProposeRollOver) -> Result<()> {
        let ProposeRollOver {
            order_id,
            timestamp,
            address,
        } = msg;

        self.send_to_maker
            .send(wire::TakerToMaker::ProposeRollOver {
                order_id,
                timestamp,
            })
            .await?;

        self.rollover_actors.insert(order_id, address);

        Ok(())
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle_connect(
        &mut self,
        Connect {
            maker_addr,
            maker_identity,
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
            let noise = noise::initiator_handshake(
                &mut connection,
                &self.identity_sk,
                &maker_identity.pk(),
            )
            .await?;

            let (read, write) = connection.into_split();
            (read, write, Arc::new(Mutex::new(noise)))
        };

        let mut read = FramedRead::new(read, wire::EncryptedJsonCodec::new(noise.clone()));
        let mut write = FramedWrite::new(write, EncryptedJsonCodec::new(noise));

        let our_version = Version::current();
        write.send(TakerToMaker::Hello(our_version.clone())).await?;

        match read
            .try_next()
            .timeout(Duration::from_secs(10))
            .await
            .with_context(|| {
                format!(
                    "Maker {} did not send Hello within 10 seconds, dropping connection",
                    maker_identity
                )
            })? {
            Ok(Some(wire::MakerToTaker::Hello(maker_version))) => {
                if our_version != maker_version {
                    self.status_sender
                        .send(ConnectionStatus::Offline {
                            reason: Some(ConnectionCloseReason::VersionMismatch {
                                taker_version: our_version.clone(),
                                maker_version: maker_version.clone(),
                            }),
                        })
                        .expect("receiver to outlive the actor");

                    bail!(
                        "Network version mismatch, we are on version {} but taker is on version {}",
                        our_version,
                        maker_version,
                    )
                }
            }
            unexpected_message => {
                bail!(
                    "Unexpected message {:?} from maker {}",
                    unexpected_message,
                    maker_identity
                )
            }
        }

        tracing::info!(address = %maker_addr, "Established connection to maker");

        let this = ctx.address().expect("self to be alive");

        let send_to_socket = send_to_socket::Actor::new(write);

        let mut tasks = Tasks::default();
        tasks.add(self.send_to_maker_ctx.attach(send_to_socket));
        tasks.add(this.attach_stream(read.map(move |item| MakerStreamMessage { item })));
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
            wire::MakerToTaker::ConfirmOrder(order_id) => {
                if self
                    .setup_actors
                    .send_fallible(&order_id, setup_taker::Accepted)
                    .await
                    .is_err()
                {
                    tracing::warn!(%order_id, "No active contract setup");
                }
            }
            wire::MakerToTaker::RejectOrder(order_id) => {
                if self
                    .setup_actors
                    .send_fallible(&order_id, setup_taker::Rejected::without_reason())
                    .await
                    .is_err()
                {
                    tracing::warn!(%order_id, "No active contract setup");
                }
            }
            wire::MakerToTaker::Protocol { order_id, msg } => {
                if self
                    .setup_actors
                    .send_fallible(&order_id, msg)
                    .await
                    .is_err()
                {
                    tracing::warn!(%order_id, "No active contract setup");
                }
            }
            wire::MakerToTaker::InvalidOrderId(order_id) => {
                if self
                    .setup_actors
                    .send_fallible(&order_id, setup_taker::Rejected::invalid_order_id())
                    .await
                    .is_err()
                {
                    tracing::warn!(%order_id, "No active contract setup");
                }
            }
            wire::MakerToTaker::Settlement { order_id, msg } => {
                if self
                    .collab_settlement_actors
                    .send(&order_id, msg)
                    .await
                    .is_err()
                {
                    tracing::warn!(%order_id, "No active collaborative settlement");
                }
            }
            wire::MakerToTaker::ConfirmRollOver {
                order_id,
                oracle_event_id,
            } => {
                if self
                    .rollover_actors
                    .send(
                        &order_id,
                        rollover_taker::RollOverAccepted { oracle_event_id },
                    )
                    .await
                    .is_err()
                {
                    tracing::warn!(%order_id, "No active rollover");
                }
            }
            wire::MakerToTaker::RejectRollOver(order_id) => {
                if self
                    .rollover_actors
                    .send(&order_id, rollover_taker::RollOverRejected)
                    .await
                    .is_err()
                {
                    tracing::warn!(%order_id, "No active rollover");
                }
            }
            wire::MakerToTaker::RollOverProtocol { order_id, msg } => {
                if self.rollover_actors.send(&order_id, msg).await.is_err() {
                    tracing::warn!(%order_id, "No active rollover");
                }
            }
            wire::MakerToTaker::CurrentOrder(msg) => {
                log_error!(self.current_order.send(CurrentOrder(msg)));
            }
            wire::MakerToTaker::Hello(_) => {
                tracing::warn!("Ignoring unexpected Hello message from maker. Hello is only expected when opening a new connection.")
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
                .send(ConnectionStatus::Offline { reason: None })
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
    maker_identity: Identity,
    maker_addresses: Vec<SocketAddr>,
) {
    loop {
        let connection_status = maker_online_status_feed_receiver.borrow().clone();
        if matches!(connection_status, ConnectionStatus::Offline { .. }) {
            tracing::debug!("No connection to the maker");
            'connect: loop {
                for address in &maker_addresses {
                    let connect_msg = Connect {
                        maker_identity,
                        maker_addr: *address,
                    };

                    if let Err(e) = connection_actor_addr
                        .send(connect_msg)
                        .await
                        .expect("Taker actor to be present")
                    {
                        tracing::warn!(%address, "Failed to establish connection: {:#}", e);
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
