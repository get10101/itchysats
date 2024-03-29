use crate::deprecated;
use crate::deprecated::protocol;
use crate::deprecated::protocol::MakerOffers;
use async_trait::async_trait;
use nonempty::NonEmpty;
use std::collections::HashSet;
use std::time::Duration;
use tokio_extras::spawn_fallible;
use tracing::Instrument;
use xtra_libp2p::endpoint;
use xtra_libp2p::libp2p::PeerId;
use xtra_libp2p::Endpoint;
use xtra_libp2p::GetConnectionStats;
use xtra_libp2p::OpenSubstream;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    endpoint: xtra::Address<Endpoint>,
    connected_peers: HashSet<PeerId>,
    latest_offers: Option<MakerOffers>,
}

impl Actor {
    pub fn new(endpoint: xtra::Address<Endpoint>) -> Self {
        Self {
            endpoint,
            connected_peers: HashSet::default(),
            latest_offers: None,
        }
    }

    #[tracing::instrument(name = "Broadcast offers to taker", skip(self, ctx))]
    async fn send_offers(&self, peer_id: PeerId, ctx: &mut xtra::Context<Self>) {
        let endpoint = self.endpoint.clone();
        let offers = self.latest_offers.clone();

        let task = async move {
            let stream = endpoint
                .send(OpenSubstream::single_protocol(
                    peer_id,
                    deprecated::PROTOCOL,
                ))
                .await??
                .await?;

            protocol::send(stream, offers).await?;

            anyhow::Ok(())
        };

        let err_handler = move |e: anyhow::Error| async move {
            match e.downcast_ref::<xtra_libp2p::Error>() {
                Some(xtra_libp2p::Error::ProtocolNotSupportedByPeer) => {
                    // Some peers may not support this protocol as listeners
                }
                Some(xtra_libp2p::Error::NegotiationFailed(_)) => {
                    tracing::debug!(%peer_id, "Failed to send offers: {e:#}")
                }
                _ => tracing::warn!(%peer_id, "Failed to send offers: {e:#}"),
            }
        };

        let this = ctx.address().expect("self to be alive");
        spawn_fallible(
            &this,
            task.instrument(tracing::Span::current()),
            err_handler,
        );
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: NewOffers, ctx: &mut xtra::Context<Self>) {
        self.latest_offers = MakerOffers::new(msg.0);

        let quiet = quiet_spans::sometimes_quiet_children();
        for peer_id in self.connected_peers.iter().copied() {
            self.send_offers(peer_id, ctx)
                .instrument(quiet.clone())
                .await
        }
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle_connection_established(
        &mut self,
        msg: endpoint::ConnectionEstablished,
        ctx: &mut xtra::Context<Self>,
    ) {
        tracing::trace!("Adding newly established connection: {:?}", msg.peer_id);
        self.connected_peers.insert(msg.peer_id);
        self.send_offers(msg.peer_id, ctx).await;
    }

    async fn handle_connection_dropped(&mut self, msg: endpoint::ConnectionDropped) {
        tracing::trace!("Remove dropped connection: {:?}", msg.peer_id);
        self.connected_peers.remove(&msg.peer_id);
    }
}

/// Instruct the `offer::maker::Actor` to broadcast to all
/// connected peers an update to the current offers.
pub struct NewOffers(NonEmpty<model::Offer>);

impl NewOffers {
    pub fn new(offers: NonEmpty<model::Offer>) -> Self {
        Self(offers)
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    #[tracing::instrument(name = "xtra_libp2p_offer::maker::Maker started", skip_all)]
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
                // prevents a continuous endless loop of restarts.
                tokio_extras::time::sleep(Duration::from_secs(2)).await;

                ctx.stop_self();
            }
        }
    }

    async fn stopped(self) -> Self::Stop {}
}
