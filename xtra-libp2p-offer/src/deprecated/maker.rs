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
use xtras::SendAsyncNext;

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

    async fn send_offers(&self, peer_id: PeerId, ctx: &mut xtra::Context<Self>) {
        let endpoint = self.endpoint.clone();
        let offers = self.latest_offers.clone();

        let span = tracing::debug_span!("Send offers", %peer_id).or_current();
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

        let this = ctx.address().expect("self to be alive");
        let err_handler = {
            let this = this.clone();
            move |e: anyhow::Error| async move {
                match e.downcast_ref::<xtra_libp2p::Error>() {
                    Some(xtra_libp2p::Error::NegotiationFailed(
                        xtra_libp2p::NegotiationError::Failed,
                    )) => {
                        // It's normal to disagree on the protocols now that we broadcast on both
                        // versions to _all_ our peers
                    }
                    Some(xtra_libp2p::Error::NoConnection(peer_id)) => {
                        this.send_async_next(NoConnection(*peer_id)).await;
                    }
                    _ => {
                        tracing::warn!(%peer_id, "Failed to send offers: {e:#}")
                    }
                }
            }
        };

        spawn_fallible(&this, task.instrument(span), err_handler);
    }

    fn remove_peer(&mut self, peer_id: PeerId) {
        if self.connected_peers.remove(&peer_id) {
            tracing::trace!(%peer_id, "Removed dropped connection");
        }
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: NewOffers, ctx: &mut xtra::Context<Self>) {
        self.latest_offers = MakerOffers::new(msg.0);

        let quiet = quiet_spans::sometimes_quiet_children();
        for peer_id in self.connected_peers.iter().copied() {
            self.send_offers(peer_id, ctx)
                .instrument(
                    quiet.in_scope(|| {
                        tracing::debug_span!("Broadcast offers to taker").or_current()
                    }),
                )
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

    async fn handle_no_connection(&mut self, msg: NoConnection) {
        self.remove_peer(msg.0);
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

struct NoConnection(PeerId);
