use crate::ConnectionStats;
use crate::Endpoint;
use crate::GetConnectionStats;
use async_trait::async_trait;
use libp2p_core::PeerId;
use std::collections::HashSet;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::message_channel::MessageChannel;
use xtra::Address;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

/// How often we check for connection changes in the endpoint
pub const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// xtra actor that takes care of dialing (connecting) to an Endpoint.
///
/// Periodically polls Endpoint to check whether connection is still active.
/// Should be used in conjunction with supervisor maintaining resilient connection.
pub struct Actor {
    tasks: Tasks,
    endpoint: Address<Endpoint>,
    connected_peers: HashSet<PeerId>,
    stop_reason: Option<Error>,
    connection_established_subscribers: Vec<Box<dyn MessageChannel<ConnectionsEstablished>>>,
    connection_dropped_subscribers: Vec<Box<dyn MessageChannel<ConnectionsDropped>>>,
}

impl Actor {
    pub fn new(
        endpoint: Address<Endpoint>,
        connection_established_subscribers: Vec<Box<dyn MessageChannel<ConnectionsEstablished>>>,
        connection_dropped_subscribers: Vec<Box<dyn MessageChannel<ConnectionsDropped>>>,
    ) -> Self {
        Self {
            tasks: Tasks::default(),
            endpoint,
            connected_peers: HashSet::default(),
            stop_reason: None,
            connection_established_subscribers,
            connection_dropped_subscribers,
        }
    }

    /// Retrieve the current connections from the endpoint
    ///
    /// Notify subscribers on new connections that were established and connections dropped.
    /// Returns error if we cannot access the Endpoint
    async fn monitor_connections(&mut self) -> Result<(), Error> {
        let ConnectionStats {
            connected_peers, ..
        } = self
            .endpoint
            .send(GetConnectionStats)
            .await
            .map_err(|_| Error::NoEndpoint)?;

        // Filter all new connections: i.e. the newly fetched contains it, but
        // self.connections does not
        let connections_established = connected_peers
            .difference(&self.connected_peers)
            .copied()
            .collect();
        self.notify_connection_established(connections_established)
            .await;

        // Filter all closed connections: i.e. self.connections contains it, but the newly
        // fetched does not
        let connections_dropped = self
            .connected_peers
            .difference(&connected_peers)
            .copied()
            .collect();
        self.notify_connection_dropped(connections_dropped).await;

        // Record the current connected peers as our new state
        self.connected_peers = connected_peers;

        Ok(())
    }

    async fn notify_connection_established(&mut self, connections_established: HashSet<PeerId>) {
        if connections_established.is_empty() {
            return;
        }

        for subscriber in &self.connection_established_subscribers {
            if let Err(e) = subscriber
                .send(ConnectionsEstablished {
                    peers: connections_established.clone(),
                })
                .await
            {
                tracing::error!(
                    "Unable to reach a subscriber that is supposed to be supervised: {e:#}"
                );
            }
        }
    }

    async fn notify_connection_dropped(&mut self, connections_dropped: HashSet<PeerId>) {
        if connections_dropped.is_empty() {
            return;
        }

        for subscriber in &self.connection_dropped_subscribers {
            if let Err(e) = subscriber
                .send(ConnectionsDropped {
                    peers: connections_dropped.clone(),
                })
                .await
            {
                tracing::error!(
                    "Unable to reach a subscriber that is supposed to be supervised: {e:#}"
                );
            }
        }
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = Error;

    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");

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
        if let Err(e) = self.monitor_connections().await {
            self.stop_reason = Some(e);
            ctx.stop();
        }
    }
}

#[derive(thiserror::Error, Copy, Clone, Debug)]
pub enum Error {
    #[error("Endpoint actor is disconnected")]
    NoEndpoint,
    #[error("Stop reason was not specified")]
    Unspecified,
}

#[derive(Clone, Copy)]
pub struct CheckConnection;

pub struct ConnectionsEstablished {
    pub peers: HashSet<PeerId>,
}

impl xtra::Message for ConnectionsEstablished {
    type Result = ();
}

pub struct ConnectionsDropped {
    pub peers: HashSet<PeerId>,
}

impl xtra::Message for ConnectionsDropped {
    type Result = ();
}
