use crate::{log_error, noise, send_to_socket, wire};
use anyhow::Result;
use futures::future::RemoteHandle;
use futures::{FutureExt, StreamExt};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::sync::watch;
use tokio_util::codec::FramedRead;
use xtra::prelude::MessageChannel;
use xtra::KeepRunning;
use xtra_productivity::xtra_productivity;

struct ConnectedState {
    last_heartbeat: SystemTime,
    _pulse_handle: RemoteHandle<()>,
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
            let socket = tokio::net::TcpSocket::new_v4().expect("Be able to create a socket");
            let mut connection = socket.connect(maker_addr).await?;
            let noise =
                noise::initiator_handshake(&mut connection, &self.identity_sk, &maker_identity_pk)
                    .await?;
            let (read, write) = connection.into_split();
            (read, write, Arc::new(Mutex::new(noise)))
        };

        let send_to_socket = send_to_socket::Actor::new(write, noise.clone());

        tokio::spawn(self.send_to_maker_ctx.attach(send_to_socket));

        let read = FramedRead::new(read, wire::EncryptedJsonCodec::new(noise))
            .map(move |item| MakerStreamMessage { item });

        let this = ctx.address().expect("self to be alive");
        tokio::spawn(this.attach_stream(read));

        let (pulse_future, pulse_remote_handle) = ctx
            .notify_interval(self.timeout, || MeasurePulse)
            .expect("we just started")
            .remote_handle();
        tokio::spawn(pulse_future);

        self.connected_state = Some(ConnectedState {
            last_heartbeat: SystemTime::now(),
            _pulse_handle: pulse_remote_handle,
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

        match msg {
            wire::MakerToTaker::Heartbeat => {
                tracing::trace!("received a heartbeat message from maker");
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
