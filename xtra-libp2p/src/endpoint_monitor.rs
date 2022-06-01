use crate::ConnectionStats;
use crate::Endpoint;
use crate::GetConnectionStats;
use async_trait::async_trait;
use libp2p_core::Multiaddr;
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

/// All subscribers to the connection monitor
pub struct Subscribers {
    connection_established: Vec<Box<dyn MessageChannel<ConnectionsEstablished>>>,
    connection_dropped: Vec<Box<dyn MessageChannel<ConnectionsDropped>>>,
    listen_address_added: Vec<Box<dyn MessageChannel<ListenAddressesAdded>>>,
    listen_address_removed: Vec<Box<dyn MessageChannel<ListenAddressesRemoved>>>,
}

impl Subscribers {
    pub fn new(
        connection_established: Vec<Box<dyn MessageChannel<ConnectionsEstablished>>>,
        connection_dropped: Vec<Box<dyn MessageChannel<ConnectionsDropped>>>,
        listen_address_added: Vec<Box<dyn MessageChannel<ListenAddressesAdded>>>,
        listen_address_removed: Vec<Box<dyn MessageChannel<ListenAddressesRemoved>>>,
    ) -> Self {
        Self {
            connection_established,
            connection_dropped,
            listen_address_added,
            listen_address_removed,
        }
    }
}

/// xtra actor that takes care of monitoring the Endpoint for changes to listen addresses and
/// connection.
///
/// Periodically polls Endpoint and dispatches the changes to subscribers.
/// Should be used in conjunction with supervisor maintaining resilient connection.
pub struct Actor {
    tasks: Tasks,
    endpoint: Address<Endpoint>,
    last_connection_stats: ConnectionStats,
    stop_reason: Option<Error>,
    subscribers: Subscribers,
}

impl Actor {
    pub fn new(endpoint: Address<Endpoint>, subscribers: Subscribers) -> Self {
        Self {
            tasks: Tasks::default(),
            endpoint,
            last_connection_stats: ConnectionStats::default(),
            stop_reason: None,
            subscribers,
        }
    }

    /// Retrieve the current connections from the endpoint
    ///
    /// Notify subscribers on new connections that were established and connections dropped.
    /// Returns error if we cannot access the Endpoint
    async fn monitor_connections(&mut self) -> Result<(), Error> {
        let connection_stats = self
            .endpoint
            .send(GetConnectionStats)
            .await
            .map_err(|_| Error::NoEndpoint)?;

        let connected_peers = &connection_stats.connected_peers;
        // Filter all new connections: i.e. the newly fetched contains it, but
        // self.connections does not
        let connections_established = connected_peers
            .difference(&self.last_connection_stats.connected_peers)
            .copied()
            .collect();
        self.notify_connection_established(connections_established)
            .await;

        // Filter all closed connections: i.e. self.connections contains it, but the newly
        // fetched does not
        let connections_dropped = self
            .last_connection_stats
            .connected_peers
            .difference(connected_peers)
            .copied()
            .collect();
        self.notify_connection_dropped(connections_dropped).await;

        let listen_addresses = &connection_stats.listen_addresses;

        let listen_addresses_added = listen_addresses
            .difference(&self.last_connection_stats.listen_addresses)
            .cloned()
            .collect();
        self.notify_listen_addresses_added(listen_addresses_added)
            .await;

        let listen_addresses_removed = self
            .last_connection_stats
            .listen_addresses
            .difference(listen_addresses)
            .cloned()
            .collect();
        self.notify_listen_addresses_removed(listen_addresses_removed)
            .await;

        // Record the current connected peers as our new state
        self.last_connection_stats = connection_stats;

        Ok(())
    }

    async fn notify_connection_established(&mut self, connections_established: HashSet<PeerId>) {
        if connections_established.is_empty() {
            return;
        }

        for subscriber in &self.subscribers.connection_established {
            if let Err(e) = subscriber
                .send(ConnectionsEstablished {
                    established: connections_established.clone(),
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
        for subscriber in &self.subscribers.connection_dropped {
            if let Err(e) = subscriber
                .send(ConnectionsDropped {
                    dropped: connections_dropped.clone(),
                })
                .await
            {
                tracing::error!(
                    "Unable to reach a subscriber that is supposed to be supervised: {e:#}"
                );
            }
        }
    }

    async fn notify_listen_addresses_added(&mut self, addresses_added: HashSet<Multiaddr>) {
        if addresses_added.is_empty() {
            return;
        }

        for subscriber in &self.subscribers.listen_address_added {
            if let Err(e) = subscriber
                .send(ListenAddressesAdded {
                    added: addresses_added.clone(),
                })
                .await
            {
                tracing::error!(
                    "Unable to reach a subscriber that is supposed to be supervised: {e:#}"
                );
            }
        }
    }

    async fn notify_listen_addresses_removed(&mut self, addresses_removed: HashSet<Multiaddr>) {
        if addresses_removed.is_empty() {
            return;
        }

        for subscriber in &self.subscribers.listen_address_removed {
            if let Err(e) = subscriber
                .send(ListenAddressesRemoved {
                    removed: addresses_removed.clone(),
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
    pub established: HashSet<PeerId>,
}

impl xtra::Message for ConnectionsEstablished {
    type Result = ();
}

pub struct ConnectionsDropped {
    pub dropped: HashSet<PeerId>,
}

impl xtra::Message for ConnectionsDropped {
    type Result = ();
}

pub struct ListenAddressesAdded {
    pub added: HashSet<Multiaddr>,
}

impl xtra::Message for ListenAddressesAdded {
    type Result = ();
}

pub struct ListenAddressesRemoved {
    pub removed: HashSet<Multiaddr>,
}

impl xtra::Message for ListenAddressesRemoved {
    type Result = ();
}
