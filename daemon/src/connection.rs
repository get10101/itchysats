use crate::address_map::AddressMap;
use crate::address_map::Stopping;
use crate::collab_settlement_taker;
use crate::model::cfd::OrderId;
use crate::model::Identity;
use crate::model::Price;
use crate::model::Timestamp;
use crate::model::Usd;
use crate::noise;
use crate::rollover_taker;
use crate::setup_taker;
use crate::taker_cfd::CurrentOrder;
use crate::tokio_ext::FutureExt;
use crate::wire;
use crate::wire::EncryptedJsonCodec;
use crate::wire::TakerToMaker;
use crate::wire::Version;
use crate::xtra_ext::LogFailure;
use crate::Tasks;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use bdk::bitcoin::Amount;
use futures::SinkExt;
use futures::StreamExt;
use futures::TryStreamExt;
use std::net::SocketAddr;
use std::time::Duration;
use std::time::SystemTime;
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_util::codec::Framed;
use xtra::prelude::MessageChannel;
use xtra::KeepRunning;
use xtra_productivity::xtra_productivity;

/// Time between reconnection attempts
const CONNECT_TO_MAKER_INTERVAL: Duration = Duration::from_secs(5);

/// The "Connected" state of our connection with the maker.
#[allow(clippy::large_enum_variant)]
enum State {
    Connected {
        last_heartbeat: SystemTime,
        write: wire::Write<wire::MakerToTaker, wire::TakerToMaker>,
        _tasks: Tasks,
    },
    Disconnected,
}

impl State {
    async fn send(&mut self, msg: wire::TakerToMaker) -> Result<()> {
        let msg_str = msg.to_string();

        let write = match self {
            State::Connected { write, .. } => write,
            State::Disconnected => {
                bail!("Cannot send {}, not connected to maker", msg_str);
            }
        };

        tracing::trace!(target: "wire", "Sending {}", msg_str);

        write
            .send(msg)
            .await
            .with_context(|| format!("Failed to send message {} to maker", msg_str))?;

        Ok(())
    }

    fn handle_incoming_heartbeat(&mut self) {
        match self {
            State::Connected { last_heartbeat, .. } => {
                *last_heartbeat = SystemTime::now();
            }
            State::Disconnected => {
                debug_assert!(false, "Received heartbeat in disconnected state")
            }
        }
    }

    fn disconnect_if_last_heartbeat_older_than(&mut self, timeout: Duration) -> bool {
        let duration_since_last_heartbeat = match self {
            State::Connected { last_heartbeat, .. } => SystemTime::now()
                .duration_since(*last_heartbeat)
                .expect("clock is monotonic"),
            State::Disconnected => return false,
        };

        if duration_since_last_heartbeat < timeout {
            return false;
        }

        *self = State::Disconnected;

        true
    }
}

pub struct Actor {
    status_sender: watch::Sender<ConnectionStatus>,
    identity_sk: x25519_dalek::StaticSecret,
    current_order: Box<dyn MessageChannel<CurrentOrder>>,
    /// Max duration since the last heartbeat until we die.
    heartbeat_timeout: Duration,
    connect_timeout: Duration,
    state: State,
    setup_actors: AddressMap<OrderId, setup_taker::Actor>,
    collab_settlement_actors: AddressMap<OrderId, collab_settlement_taker::Actor>,
    rollover_actors: AddressMap<OrderId, rollover_taker::Actor>,
}

pub struct Connect {
    pub maker_identity: Identity,
    pub maker_addresses: Vec<SocketAddr>,
}

#[derive(thiserror::Error, Debug)]
pub enum ConnectError {
    #[error("Connection got closed")]
    Closed(ConnectionCloseReason),
    #[error("Made {number} attempts, all failed")]
    AllAttemptsFailed { number: usize },
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

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ConnectionCloseReason {
    #[error(
        "Network version mismatch, taker is on {taker_version} and maker is on {maker_version}"
    )]
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
        Self {
            status_sender,
            identity_sk,
            current_order: current_order.clone_channel(),
            heartbeat_timeout: hearthbeat_timeout,
            state: State::Disconnected,
            setup_actors: AddressMap::default(),
            connect_timeout,
            collab_settlement_actors: AddressMap::default(),
            rollover_actors: AddressMap::default(),
        }
    }

    async fn connect(
        &self,
        maker_identity: Identity,
        maker_addr: SocketAddr,
        ctx: &mut xtra::Context<Self>,
    ) -> Result<State, FailedToConnect> {
        tracing::debug!(address = %maker_addr, "Connecting to maker");

        let (mut write, mut read) = {
            let mut connection = TcpStream::connect(&maker_addr)
                .timeout(self.connect_timeout)
                .await
                .map_err(|_| FailedToConnect::Timeout {
                    duration: self.connect_timeout.as_secs(),
                })?
                .with_context(|| format!("Failed to connect to {}", maker_addr))?;
            let noise = noise::initiator_handshake(
                &mut connection,
                &self.identity_sk,
                &maker_identity.pk(),
            )
            .await?;

            Framed::new(connection, EncryptedJsonCodec::new(noise)).split()
        };

        let our_version = Version::current();
        write
            .send(TakerToMaker::Hello(our_version.clone()))
            .await
            .context("Failed to send `Hello`")?;

        match read
            .try_next()
            .timeout(Duration::from_secs(10))
            .await
            .context("No message after 10 seconds")
            .map_err(|e| FailedToConnect::NoHelloMessage { source: e })?
        {
            Ok(Some(wire::MakerToTaker::Hello(maker_version))) => {
                if our_version != maker_version {
                    return Err(FailedToConnect::VersionMismatch {
                        taker: our_version,
                        maker: maker_version,
                    });
                }
            }
            Ok(Some(msg)) => {
                return Err(FailedToConnect::NoHelloMessage {
                    source: anyhow!("Expected `Hello` but got {}", msg),
                });
            }
            Ok(None) => {
                return Err(FailedToConnect::Other(anyhow!("EOF")));
            }
            Err(e) => {
                return Err(FailedToConnect::Other(e));
            }
        }

        tracing::info!(address = %maker_addr, "Established connection to maker");

        let this = ctx.address().expect("self to be alive");

        let mut tasks = Tasks::default();
        tasks.add(this.attach_stream(read.map(move |item| MakerStreamMessage { item })));
        tasks.add(
            ctx.notify_interval(self.heartbeat_timeout, || MeasurePulse)
                .expect("we just started"),
        );

        Ok(State::Connected {
            last_heartbeat: SystemTime::now(),
            write,
            _tasks: tasks,
        })
    }
}

#[derive(thiserror::Error, Debug)]
enum FailedToConnect {
    #[error("Connection attempt timed out after {duration} seconds")]
    Timeout { duration: u64 },
    #[error("Maker did not send a `Hello` message")]
    NoHelloMessage { source: anyhow::Error },
    #[error("Network version mismatch; ours is {taker}, theirs is {maker}")]
    VersionMismatch { taker: Version, maker: Version },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[xtra_productivity(message_impl = false)]
impl Actor {
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
    async fn handle_taker_to_maker(&mut self, message: wire::TakerToMaker) {
        if let Err(e) = self.state.send(message).await {
            tracing::warn!("{:#}", e);
        }
    }

    async fn handle_take_order(&mut self, msg: TakeOrder) -> Result<()> {
        self.state
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

        self.state
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

        self.state
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
            maker_addresses,
            maker_identity,
        }: Connect,
        ctx: &mut xtra::Context<Self>,
    ) -> Result<(), ConnectError> {
        let num_addresses = maker_addresses.len();

        for address in maker_addresses {
            match self.connect(maker_identity, address, ctx).await {
                Ok(state) => {
                    self.state = state;
                    self.status_sender
                        .send(ConnectionStatus::Online)
                        .expect("receiver to outlive the actor");

                    return Ok(());
                }
                Err(FailedToConnect::VersionMismatch { taker, maker }) => {
                    let reason = ConnectionCloseReason::VersionMismatch {
                        taker_version: taker,
                        maker_version: maker,
                    };

                    let _ = self.status_sender.send(ConnectionStatus::Offline {
                        reason: Some(reason.clone()),
                    });

                    return Err(ConnectError::Closed(reason));
                }
                Err(e) => {
                    tracing::warn!(%address, "Failed to establish connection: {:#}", anyhow!(e)); // format as anyhow to get entire backtrace
                    continue;
                }
            }
        }

        return Err(ConnectError::AllAttemptsFailed {
            number: num_addresses,
        });
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

        tracing::trace!(target: "wire", "Received {}", msg);

        match msg {
            wire::MakerToTaker::Heartbeat => {
                self.state.handle_incoming_heartbeat();
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
                let _ = self
                    .current_order
                    .send(CurrentOrder(msg))
                    .log_failure("Failed to forward current order from maker")
                    .await;
            }
            wire::MakerToTaker::Hello(_) => {
                tracing::warn!("Ignoring unexpected Hello message from maker. Hello is only expected when opening a new connection.")
            }
        }
        KeepRunning::Yes
    }

    fn handle_measure_pulse(&mut self, _: MeasurePulse) {
        if self
            .state
            .disconnect_if_last_heartbeat_older_than(self.heartbeat_timeout)
        {
            self.status_sender
                .send(ConnectionStatus::Offline { reason: None })
                .expect("watch receiver to outlive the actor");
        }
    }
}

impl xtra::Actor for Actor {}

// TODO: Move the reconnection logic inside the connection::Actor instead of
// depending on a watch channel
pub async fn connect(
    mut maker_online_status_feed_receiver: watch::Receiver<ConnectionStatus>,
    address: xtra::Address<Actor>,
    maker_identity: Identity,
    maker_addresses: Vec<SocketAddr>,
) {
    loop {
        let connection_status = maker_online_status_feed_receiver.borrow().clone();
        if matches!(connection_status, ConnectionStatus::Offline { .. }) {
            tracing::debug!("No connection to the maker");

            let connect = || async {
                address
                    .send(Connect {
                        maker_identity,
                        maker_addresses: maker_addresses.clone(),
                    })
                    .await
                    .expect("Taker actor to be present")
            };

            while let Err(e) = connect().await {
                tracing::warn!("{:#}", e);

                tokio::time::sleep(CONNECT_TO_MAKER_INTERVAL).await;
            }
        }
        maker_online_status_feed_receiver
            .changed()
            .await
            .expect("watch channel should outlive the future");
    }
}
