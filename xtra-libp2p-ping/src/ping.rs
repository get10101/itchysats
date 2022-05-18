use crate::protocol;
use crate::PROTOCOL_NAME;
use conquer_once::Lazy;
use prometheus::register_histogram;
use prometheus::Histogram;
use std::collections::HashMap;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::async_trait;
use xtra::Address;
use xtra::Context;
use xtra_libp2p::libp2p::PeerId;
use xtra_libp2p::Endpoint;
use xtra_libp2p::GetConnectionStats;
use xtra_libp2p::OpenSubstream;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

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
pub(crate) struct GetLatency(pub PeerId);

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
                    let latency = protocol::send(stream).await?;

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

        let latency_seconds = (latency_milliseconds as f64) / 1000.0;
        PEER_LATENCY_HISTOGRAM.observe(latency_seconds);
    }

    async fn handle(&mut self, GetLatency(peer): GetLatency) -> Option<Duration> {
        return self.latencies.get(&peer).copied();
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
