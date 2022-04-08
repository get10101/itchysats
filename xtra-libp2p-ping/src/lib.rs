use conquer_once::Lazy;
use libp2p_xtra::libp2p::PeerId;
use libp2p_xtra::Endpoint;
use libp2p_xtra::GetConnectionStats;
use libp2p_xtra::NewInboundSubstream;
use libp2p_xtra::OpenSubstream;
use prometheus::register_histogram;
use prometheus::Histogram;
use std::collections::HashMap;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::async_trait;
use xtra::Address;
use xtra::Context;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

/// The name of the official ipfs/libp2p ping protocol.
///
/// Using this indicates that we are wire-compatible with other libp2p/ipfs nodes.
pub const PROTOCOL_NAME: &str = "/ipfs/ping/1.0.0";

/// An actor implementing the official ipfs/libp2p ping protocol.
///
/// The ping protocol serves two purposes:
///
/// 1. To measure the latency to other peers.
/// 2. To prevent an otherwise seldom-utilised connection from being closed by intermediary network
/// devices along the connection pathway.
///
/// When constructed with a `ping_interval`, the actor will request all connected peers from the
/// provided [`Endpoint`] and ping all peers.
///
/// This actor also implements the listening end of the ping protocol and will correctly handle
/// incoming pings even without a `ping_interval` set. This is useful if an application wants to
/// allow other peers in the network to measure their latency but is not interested in measuring
/// latencies itself or keeping connections alive otherwise.
pub struct Actor {
    endpoint: Address<Endpoint>,
    ping_interval: Option<Duration>,
    tasks: Tasks,
    latencies: HashMap<PeerId, Duration>,
}

impl Actor {
    pub fn new(endpoint: Address<Endpoint>, ping_interval: Option<Duration>) -> Self {
        Self {
            endpoint,
            ping_interval,
            tasks: Tasks::default(),
            latencies: HashMap::default(),
        }
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn started(&mut self, ctx: &mut Context<Self>) {
        let this = ctx.address().expect("we just started");

        if let Some(interval) = self.ping_interval {
            self.tasks.add(this.send_interval(interval, || Ping));
        }
    }

    async fn stopped(self) -> Self::Stop {}
}

/// Private message to ping all connected peers.
struct Ping;

/// Private message to record latency of a peer.
struct RecordLatency {
    peer: PeerId,
    latency: Duration,
}

/// Private message to get the latency of a peer.
///
/// Primarily used for testing. May be exposed publicly at some point.
struct GetLatency(pub PeerId);

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: Ping, ctx: &mut Context<Self>) {
        let connection_stats = match self.endpoint.send(GetConnectionStats).await {
            Ok(connection_stats) => connection_stats,
            Err(_) => {
                tracing::warn!("Cannot ping peers because `Endpoint` actor is down");
                return;
            }
        };

        self.latencies.clear();

        for peer in connection_stats.connected_peers {
            let endpoint = self.endpoint.clone();
            let this = ctx.address().expect("we are alive");

            self.tasks.add_fallible(
                async move {
                    tracing::trace!(%peer, "Sending ping");

                    let stream = endpoint
                        .send(OpenSubstream::single_protocol(peer, PROTOCOL_NAME))
                        .await??;
                    let latency = ping::send(stream).await?;

                    this.send(RecordLatency {
                        peer,
                        latency
                    }).await?;

                    anyhow::Ok(())
                },
                move |e| async move { tracing::debug!(%peer, "Outbound ping protocol failed: {e:#}") },
            );
        }
    }

    async fn handle(&mut self, msg: RecordLatency) {
        let RecordLatency { peer, latency } = msg;

        self.latencies.insert(peer, latency);

        let latency_milliseconds = latency.as_millis();

        tracing::trace!(%peer, %latency_milliseconds, "Received pong");

        let latency_seconds = (latency_milliseconds as f64) * 1000.0;
        PEER_LATENCY_HISTOGRAM.observe(latency_seconds);
    }

    async fn handle(&mut self, GetLatency(peer): GetLatency) -> Option<Duration> {
        return self.latencies.get(&peer).copied();
    }
}

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle(&mut self, message: NewInboundSubstream) {
        let NewInboundSubstream { stream, peer } = message;

        let future = ping::recv(stream);

        self.tasks.add_fallible(future, move |e| async move {
            tracing::debug!(%peer, "Inbound ping protocol failed: {e}");
        });
    }
}

/// A histogram tracking the latency to all our connected peers.
///
/// There are two things to note about the design of this metric.
///
/// 1. We are not using any labels. It is tempting to track the latency _per peer_, however creating
/// labels for unbounded sets of values (like user IDs) is an anti-pattern (see https://prometheus.io/docs/practices/naming/#labels).
/// 2. We assume most latencies will be in the order of 10-100 milliseconds which is why most of our
/// histogram buckets focus on this range.
static PEER_LATENCY_HISTOGRAM: Lazy<Histogram> = Lazy::new(|| {
    register_histogram!(
        "p2p_ping_latency_seconds",
        "The latency of ping messages to all connected peers in seconds.",
        vec![
            0.01, 0.02, 0.03, 0.04, 0.05, 0.06, 0.07, 0.08, 0.09, 0.1, 0.15, 0.2, 0.3, 0.5, 0.75,
            1.0, 2.0, 5.0
        ]
    )
    .unwrap()
});

/// The actual protocol functions for sending ping messages.
///
/// Insbired by https://github.com/libp2p/rust-libp2p/blob/102509afe3a3b984e43a88dbe4de935fde36f319/protocols/ping/src/protocol.rs#L82-L113.
mod ping {
    pub const SIZE: usize = 32;

    use futures::AsyncReadExt;
    use futures::AsyncWriteExt;
    use libp2p_xtra::Substream;
    use rand::distributions;
    use rand::thread_rng;
    use rand::Rng;
    use std::io;
    use std::time::Duration;
    use std::time::Instant;

    /// Sends a ping and waits for the pong.
    pub async fn send(mut stream: Substream) -> io::Result<Duration> {
        let payload: [u8; SIZE] = thread_rng().sample(distributions::Standard);
        stream.write_all(&payload).await?;
        stream.flush().await?;

        let started = Instant::now();

        let mut recv_payload = [0u8; SIZE];
        stream.read_exact(&mut recv_payload).await?;

        if recv_payload != payload {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Ping payload mismatch",
            ));
        }

        Ok(started.elapsed())
    }

    /// Waits for a ping and sends a pong.
    pub async fn recv(mut stream: Substream) -> io::Result<()> {
        let mut payload = [0u8; SIZE];
        stream.read_exact(&mut payload).await?;
        stream.write_all(&payload).await?;
        stream.flush().await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p_xtra::libp2p::identity::Keypair;
    use libp2p_xtra::libp2p::multiaddr::Protocol;
    use libp2p_xtra::libp2p::transport::MemoryTransport;
    use libp2p_xtra::libp2p::Multiaddr;
    use libp2p_xtra::Connect;
    use libp2p_xtra::ListenOn;
    use xtra::message_channel::StrongMessageChannel;
    use xtra::spawn::TokioGlobalSpawnExt;
    use xtra::Actor as _;

    #[tokio::test]
    async fn latency_to_peer_is_recorded() {
        tracing_subscriber::fmt()
            .with_env_filter("xtra_libp2p_ping=trace")
            .with_test_writer()
            .init();

        let (alice_peer_id, alice_ping_actor, alice_endpoint) = create_endpoint_with_ping();
        let (bob_peer_id, bob_ping_actor, bob_endpoint) = create_endpoint_with_ping();

        alice_endpoint
            .send(ListenOn(Multiaddr::empty().with(Protocol::Memory(1000))))
            .await
            .unwrap();
        bob_endpoint
            .send(Connect(
                Multiaddr::empty()
                    .with(Protocol::Memory(1000))
                    .with(Protocol::P2p(alice_peer_id.into())),
            ))
            .await
            .unwrap()
            .unwrap();

        tokio::time::sleep(Duration::from_secs(2)).await;

        let alice_to_bob_latency = alice_ping_actor
            .send(GetLatency(bob_peer_id))
            .await
            .unwrap()
            .unwrap();
        let bob_to_alice_latency = bob_ping_actor
            .send(GetLatency(alice_peer_id))
            .await
            .unwrap()
            .unwrap();

        assert!(!alice_to_bob_latency.is_zero());
        assert!(!bob_to_alice_latency.is_zero());
    }

    fn create_endpoint_with_ping() -> (PeerId, Address<Actor>, Address<Endpoint>) {
        let (endpoint_address, endpoint_context) = Context::new(None);

        let id = Keypair::generate_ed25519();
        let ping_address = Actor::new(endpoint_address.clone(), Some(Duration::from_secs(1)))
            .create(None)
            .spawn_global();
        let endpoint = Endpoint::new(
            MemoryTransport::default(),
            id.clone(),
            Duration::from_secs(10),
            [(PROTOCOL_NAME, ping_address.clone_channel())],
        );

        #[allow(clippy::disallowed_method)]
        tokio::spawn(endpoint_context.run(endpoint));

        (id.public().to_peer_id(), ping_address, endpoint_address)
    }
}
