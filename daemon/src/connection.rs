use crate::{log_error, noise, send_to_socket, wire, Tasks};
use anyhow::Result;
use futures::StreamExt;
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
    timeout: Duration,
    connected_state: Option<ConnectedState>,
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

impl Actor {
    pub fn new(
        status_sender: watch::Sender<ConnectionStatus>,
        maker_to_taker: Box<dyn MessageChannel<wire::MakerToTaker>>,
        identity_sk: x25519_dalek::StaticSecret,
        timeout: Duration,
    ) -> Self {
        let (send_to_maker_addr, send_to_maker_ctx) = xtra::Context::new(None);

        Self {
            status_sender,
            send_to_maker: Box::new(send_to_maker_addr),
            send_to_maker_ctx,
            identity_sk,
            maker_to_taker,
            timeout,
            connected_state: None,
        }
    }
}

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle_taker_to_maker(&mut self, message: wire::TakerToMaker) {
        log_error!(self.send_to_maker.send(message));
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
        let (read, write, noise) = {
            let mut connection = TcpStream::connect(&maker_addr).await?;
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
            ctx.notify_interval(self.timeout, || MeasurePulse)
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

        if time_since_last_heartbeat > self.timeout {
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
            tracing::info!("No connection to the maker, attempting to connect:");
            'connect: loop {
                for address in &maker_addresses {
                    tracing::trace!("Connecting to {}", address);

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
                    tracing::debug!("Connection established");
                    break 'connect;
                }

                tracing::debug!(
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
