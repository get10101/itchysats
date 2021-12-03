use crate::address_map::{AddressMap, Stopping};
use crate::model::cfd::OrderId;
use crate::model::{Identity, Price, Timestamp, Usd};
use crate::tokio_ext::FutureExt;
use crate::wire::{EncryptedJsonCodec, TakerToMaker, Version};
use crate::{collab_settlement_taker, log_error, noise, send_to_socket, setup_taker, wire, Tasks};
use anyhow::{bail, Context, Result};
use bdk::bitcoin::Amount;
use futures::{SinkExt, StreamExt, TryStreamExt};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_util::codec::{FramedRead, FramedWrite};
use xtra::prelude::MessageChannel;
use xtra::KeepRunning;
use xtra_productivity::xtra_productivity;

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

#[derive(Clone)]
pub struct Connect {
    pub maker_identity: Identity,
    pub maker_addr: SocketAddr,
}

#[derive(Clone)]
pub struct AttemptAll {
    pub maker_identity: Identity,
    pub maker_addresses: Vec<SocketAddr>,
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

impl Actor {
    async fn connect_internal(
        &mut self,
        maker_addr: SocketAddr,
        maker_identity: Identity,
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
        self.connect_internal(maker_addr, maker_identity, ctx).await
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
            wire::MakerToTaker::Hello(_) => {
                tracing::warn!("Ignoring unexpected Hello message from maker. Hello is only expected when opening a new connection.")
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
                .send(ConnectionStatus::Offline { reason: None })
                .expect("watch receiver to outlive the actor");
            self.connected_state = None;
        }
    }

    async fn handle_attempt_all(
        &mut self,
        AttemptAll {
            maker_addresses,
            maker_identity: maker_identity_pk,
        }: AttemptAll,
        ctx: &mut xtra::Context<Self>,
    ) -> Result<()> {
        for maker_addr in maker_addresses {
            match self
                .connect_internal(maker_addr, maker_identity_pk, ctx)
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::debug!("{:#}", e);
                }
            }
        }

        bail!("All connection attempts failed")
    }
}

impl xtra::Actor for Actor {}
