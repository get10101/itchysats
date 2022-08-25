use crate::current::protocol;
use crate::current::PROTOCOL;
use async_trait::async_trait;
use model::ContractSymbol;
use model::Position;
use std::collections::HashMap;
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
    current_offers: Offers,
}

impl Actor {
    pub fn new(endpoint: xtra::Address<Endpoint>) -> Self {
        Self {
            endpoint,
            connected_peers: HashSet::default(),
            current_offers: Offers::default(),
        }
    }

    async fn send_offers(
        &self,
        peer_id: PeerId,
        offers: Vec<model::Offer>,
        ctx: &mut xtra::Context<Self>,
    ) {
        let endpoint = self.endpoint.clone();

        let span = tracing::debug_span!("Send offers", %peer_id).or_current();
        let task = async move {
            let stream = endpoint
                .send(OpenSubstream::single_protocol(peer_id, PROTOCOL))
                .await??
                .await?;

            protocol::send(stream, offers.into()).await?;

            anyhow::Ok(())
        };

        let err_handler = move |e: anyhow::Error| async move {
            if let Some(xtra_libp2p::Error::NegotiationFailed(
                xtra_libp2p::NegotiationError::Failed,
            )) = e.downcast_ref::<xtra_libp2p::Error>()
            {
                // It's normal to disagree on the protocols now that we broadcast on both versions
                // to _all_ our peers
            } else {
                tracing::warn!(%peer_id, "Failed to send offers: {e:#}")
            }
        };

        let this = ctx.address().expect("self to be alive");
        spawn_fallible(&this, task.instrument(span), err_handler);
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: NewOffers, ctx: &mut xtra::Context<Self>) {
        self.current_offers.update(msg.0.clone());

        let quiet = quiet_spans::sometimes_quiet_children();
        for peer_id in self.connected_peers.iter().copied() {
            self.send_offers(peer_id, msg.0.clone(), ctx)
                .instrument(quiet.in_scope(|| {
                    tracing::debug_span!("Broadcast offers to taker (libp2p)").or_current()
                }))
                .await
        }
    }

    async fn handle(&mut self, _: GetLatestOffers) -> Vec<model::Offer> {
        self.current_offers.to_vec()
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
        self.send_offers(msg.peer_id, self.current_offers.to_vec(), ctx)
            .await;
    }

    async fn handle_connection_dropped(&mut self, msg: endpoint::ConnectionDropped) {
        tracing::trace!("Remove dropped connection: {:?}", msg.peer_id);
        self.connected_peers.remove(&msg.peer_id);
    }
}

/// Instruct the `offer::maker::Actor` to broadcast to all
/// connected peers an update to the current offers.
pub struct NewOffers(Vec<model::Offer>);

impl NewOffers {
    pub fn new(offers: Vec<model::Offer>) -> Self {
        Self(offers)
    }
}

#[derive(Clone, Copy)]
pub struct GetLatestOffers;

#[derive(Clone, Default)]
struct Offers(HashMap<(ContractSymbol, Position), model::Offer>);

impl Offers {
    fn update(&mut self, offers: Vec<model::Offer>) {
        for offer in offers.into_iter() {
            self.0
                .insert((offer.contract_symbol, offer.position_maker), offer);
        }
    }

    fn to_vec(&self) -> Vec<model::Offer> {
        self.0.iter().map(|(_, offer)| offer).cloned().collect()
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
