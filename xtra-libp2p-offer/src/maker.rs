use crate::protocol;
use crate::PROTOCOL_NAME;
use async_trait::async_trait;
use model::MakerOffers;
use std::collections::HashSet;
use std::time::Duration;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor as _;
use xtra_libp2p::endpoint;
use xtra_libp2p::libp2p::PeerId;
use xtra_libp2p::Endpoint;
use xtra_libp2p::GetConnectionStats;
use xtra_libp2p::OpenSubstream;
use xtra_productivity::xtra_productivity;
use xtras::spawner;
use xtras::spawner::SpawnFallible;
use xtras::SendAsyncSafe;

pub struct Actor {
    endpoint: xtra::Address<Endpoint>,
    connected_peers: HashSet<PeerId>,
    spawner: xtra::Address<spawner::Actor>,
    latest_offers: Option<MakerOffers>,
}

impl Actor {
    pub fn new(endpoint: xtra::Address<Endpoint>) -> Self {
        let spawner = spawner::Actor::new().create(None).spawn_global();

        Self {
            endpoint,
            connected_peers: HashSet::default(),
            spawner,
            latest_offers: None,
        }
    }

    async fn send_offers(&self, peer: PeerId) {
        let endpoint = self.endpoint.clone();
        let offers = self.latest_offers.clone();

        let task = async move {
            tracing::debug!(%peer, ?offers, "Sending offers");

            let stream = endpoint
                .send(OpenSubstream::single_protocol(peer, PROTOCOL_NAME))
                .await??;

            protocol::send(stream, offers).await?;

            anyhow::Ok(())
        };

        let err_handler =
            move |e| async move { tracing::debug!(%peer, "Failed to send offers: {e:#}") };

        if let Err(e) = self
            .spawner
            .send_async_safe(SpawnFallible::new(task, err_handler))
            .await
        {
            tracing::warn!("Failed to spawn task to send offers: {e:#}");
        };
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: NewOffers) {
        self.latest_offers = msg.0;

        for peer in self.connected_peers.iter().copied() {
            self.send_offers(peer).await
        }
    }
}

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle_connection_established(&mut self, msg: endpoint::ConnectionEstablished) {
        tracing::trace!("Adding newly established connection: {:?}", msg.peer);
        self.connected_peers.insert(msg.peer);
        self.send_offers(msg.peer).await;
    }

    async fn handle_connection_dropped(&mut self, msg: endpoint::ConnectionDropped) {
        tracing::trace!("Remove dropped connection: {:?}", msg.peer);
        self.connected_peers.remove(&msg.peer);
    }
}

/// Instruct the `offer::maker::Actor` to broadcast to all
/// connected peers an update to the current offers.
pub struct NewOffers(Option<MakerOffers>);

impl NewOffers {
    pub fn new(offers: Option<MakerOffers>) -> Self {
        Self(offers)
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        match self.endpoint.send(GetConnectionStats).await {
            Ok(connection_stats) => self
                .connected_peers
                .extend(connection_stats.connected_peers),
            Err(e) => {
                tracing::error!(
                    "Unable to receive connection stats from the endpoint upon startup: {e:#}"
                );

                // This code path should not be hit, but in case we run into an error this sleep
                // prevents a continuous endless loup of restarts.
                tokio::time::sleep(Duration::from_secs(2)).await;

                ctx.stop_self(); // TODO(restioson) stop
            }
        }
    }

    async fn stopped(self) -> Self::Stop {}
}
