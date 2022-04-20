use crate::Connect;
use crate::ConnectionStats;
use crate::Endpoint;
use crate::GetConnectionStats;
use anyhow::anyhow;
use async_trait::async_trait;
use libp2p_core::Multiaddr;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::Address;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

/// How often we check whether we are still connected
pub const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// xtra actor that takes care of dialing (connecting) to an Endpoint.
///
/// Periodically polls Endpoint to check whether connection is still active.
/// Should be used in conjunction with supervisor maintaining resilient connection.
pub struct Actor {
    tasks: Tasks,
    endpoint: Address<Endpoint>,
    connect_address: Multiaddr,
    stop_reason: Option<Error>,
}

impl Actor {
    pub fn new(endpoint: Address<Endpoint>, connect_address: Multiaddr) -> Self {
        Self {
            tasks: Tasks::default(),
            endpoint,
            connect_address,
            stop_reason: None,
        }
    }

    /// Returns error if we cannot access Endpoint or the connect address is not
    /// present inside Endpoint.
    async fn check_connection_active_in_endpoint(&self) -> Result<(), Error> {
        let ConnectionStats {
            listen_addresses, ..
        } = self
            .endpoint
            .send(GetConnectionStats)
            .await
            .map_err(|_| Error::NoEndpoint)?;

        listen_addresses
            .contains(&self.connect_address)
            .then(|| ())
            .ok_or(Error::ConnectionDropped)
    }

    async fn connect(&self) -> Result<(), Error> {
        self.endpoint
            .send(Connect(self.connect_address.clone()))
            .await
            .map_err(|_| Error::NoEndpoint)?
            .map_err(|e| Error::Failed { source: anyhow!(e) })
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = Error;

    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");

        if let Err(e) = self.connect().await {
            self.stop_reason = Some(e);
            ctx.stop();
        }

        // Only start checking the connection after it had enough time to be established
        tokio::time::sleep(CHECK_INTERVAL).await;
        self.tasks
            .add(this.send_interval(CHECK_INTERVAL, || CheckConnection));
    }

    async fn stopped(self) -> Self::Stop {
        self.stop_reason.unwrap_or(Error::Unspecified)
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: Error, ctx: &mut xtra::Context<Self>) {
        self.stop_reason = Some(msg);
        ctx.stop();
    }

    async fn handle(&mut self, _msg: CheckConnection, ctx: &mut xtra::Context<Self>) {
        if let Err(e) = self.check_connection_active_in_endpoint().await {
            self.stop_reason = Some(e);
            ctx.stop();
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
    #[error("Stop reason was not specified")]
    Unspecified,
}

#[derive(Clone, Copy)]
pub struct CheckConnection;
