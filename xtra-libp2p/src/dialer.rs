use crate::endpoint;
use crate::multiaddress_ext::MultiaddrExt;
use crate::Connect;
use crate::Endpoint;
use anyhow::anyhow;
use async_trait::async_trait;
use libp2p_core::Multiaddr;
use libp2p_core::PeerId;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::Address;
use xtra_productivity::xtra_productivity;

/// If we're not connected by this time, stop the actor.
pub const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// xtra actor that takes care of dialing (connecting) to an Endpoint.
///
/// Periodically polls Endpoint to check whether connection is still active.
/// Should be used in conjunction with supervisor maintaining resilient connection.
pub struct Actor {
    tasks: Tasks,
    endpoint: Address<Endpoint>,
    connect_address: Multiaddr,
    connected: bool,
    listener_peer_id: Option<PeerId>,
    stop_reason: Option<Error>,
}

impl Actor {
    pub fn new(endpoint: Address<Endpoint>, connect_address: Multiaddr) -> Self {
        Self {
            tasks: Tasks::default(),
            endpoint,
            connect_address,
            connected: false,
            listener_peer_id: None,
            stop_reason: None,
        }
    }

    async fn connect(&self) -> Result<(), Error> {
        self.endpoint
            .send(Connect(self.connect_address.clone()))
            .await
            .map_err(|_| Error::NoEndpoint)?
            .map_err(|e| Error::Failed { source: anyhow!(e) })
    }

    fn stop_with_error(&mut self, e: Error, ctx: &mut xtra::Context<Self>) {
        self.stop_reason = Some(e);
        ctx.stop();
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = Error;

    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        match self
            .connect_address
            .clone()
            .extract_peer_id()
            .ok_or(Error::InvalidPeerId)
        {
            Ok(peer_id) => self.listener_peer_id = Some(peer_id),
            Err(e) => {
                self.stop_with_error(e, ctx);
            }
        }

        if let Err(e) = self.connect().await {
            tracing::warn!("Failed to connect to maker: {e:#}");

            self.stop_with_error(e, ctx);
        }

        let this = ctx.address().expect("self to be alive");
        self.tasks.add(async move {
            tokio::time::sleep(CONNECTION_TIMEOUT).await;
            this.send(StopIfNotConnected)
                .await
                .expect("to deliver stop message");
        })
    }

    async fn stopped(self) -> Self::Stop {
        self.stop_reason.unwrap_or(Error::Unspecified)
    }
}

impl Actor {
    fn peer_id(&self) -> PeerId {
        self.listener_peer_id
            .expect("to always have peer id if successfully started")
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _msg: StopIfNotConnected, ctx: &mut xtra::Context<Self>) {
        if !self.connected {
            self.stop_with_error(
                Error::Failed {
                    source: anyhow!("Did not connect in time"),
                },
                ctx,
            )
        }
    }

    async fn handle(&mut self, msg: Error, ctx: &mut xtra::Context<Self>) {
        self.stop_with_error(msg, ctx);
    }
}

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle(&mut self, msg: endpoint::ConnectionEstablished) {
        if msg.peer == self.peer_id() {
            self.connected = true;
        }
    }

    async fn handle(&mut self, msg: endpoint::ConnectionDropped, ctx: &mut xtra::Context<Self>) {
        if msg.peer == self.peer_id() {
            self.connected = false;
            self.stop_with_error(Error::ConnectionDropped, ctx);
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Dialer failed")]
    Failed { source: anyhow::Error },
    #[error("Endpoint actor is disconnected")]
    NoEndpoint,
    #[error("Connection dropped from endpoint")]
    ConnectionDropped,
    #[error("Invalid Peer Id")]
    InvalidPeerId,
    #[error("Stop reason was not specified")]
    Unspecified,
}

struct StopIfNotConnected;
