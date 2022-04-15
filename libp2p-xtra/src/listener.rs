use crate::ConnectionStats;
use crate::Endpoint;
use crate::GetConnectionStats;
use crate::ListenOn;
use anyhow::anyhow;
use anyhow::Result;
use async_trait::async_trait;
use libp2p_core::Multiaddr;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::Address;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

/// How often we check whether we are still connected
pub const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// xtra actor that taker care of listening for incoming connections to the Endpoint.
///
/// Periodically polls Endpoint to check whether connection is still active.
/// Should be used in conjunction with supervisor for continuous and resilient listening.
pub struct Actor {
    tasks: Tasks,
    endpoint: Address<Endpoint>,
    listen_address: Multiaddr,
    /// Contains the reason we are stopping.
    stop_reason: Option<Error>,
}

impl Actor {
    pub fn new(endpoint: Address<Endpoint>, listen_address: Multiaddr) -> Self {
        Self {
            tasks: Tasks::default(),
            endpoint,
            listen_address,
            stop_reason: None,
        }
    }

    async fn check_connection_active_in_endpoint(&self) -> Result<()> {
        let ConnectionStats {
            listen_addresses, ..
        } = self
            .endpoint
            .send(GetConnectionStats)
            .await
            .map_err(|_| Error::NoEndpoint)?;

        listen_addresses
            .contains(&self.listen_address)
            .then(|| ())
            .ok_or_else(|| anyhow!("Listener address not active in Endpoint"))
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = Error;

    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");

        let endpoint = self.endpoint.clone();
        let listen_address = self.listen_address.clone();

        if let Err(e) = endpoint.send(ListenOn(listen_address)).await {
            self.stop_reason = Some(Error::Failed { source: anyhow!(e) });
            ctx.stop()
        };

        // Only start checking the connection after it had enough time to be registered
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
        if self.check_connection_active_in_endpoint().await.is_err() {
            self.stop_reason = Some(Error::ConnectionDropped);
            ctx.stop();
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Listener failed")]
    Failed { source: anyhow::Error },
    #[error("Endpoint actor is disconnected")]
    NoEndpoint,
    #[error("Connection dropped from endpoint")]
    ConnectionDropped,
    #[error("Stop reason was not specified")]
    Unspecified,
}

#[derive(Clone, Copy)]
struct CheckConnection;
