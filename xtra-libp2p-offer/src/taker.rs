use crate::protocol;
use async_trait::async_trait;
use model::MakerOffers;
use tracing::Instrument;
use xtra::prelude::MessageChannel;
use xtra_libp2p::NewInboundSubstream;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    maker_offers: MessageChannel<LatestMakerOffers, ()>,
}

impl Actor {
    pub fn new(maker_offers: MessageChannel<LatestMakerOffers, ()>) -> Self {
        Self { maker_offers }
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: NewInboundSubstream, ctx: &mut xtra::Context<Self>) {
        let NewInboundSubstream { peer, stream } = msg;
        let maker_offers = self.maker_offers.clone();

        let this = ctx.address().expect("self to be alive");

        let task = async move {
            let offers = protocol::recv(stream).await?;
            let span = tracing::debug_span!("Received new offers from maker", %peer, ?offers);
            maker_offers
                .send(LatestMakerOffers(offers))
                .instrument(span)
                .await?;

            anyhow::Ok(())
        };

        let err_handler =
            move |e| async move { tracing::warn!(%peer, "Failed to process maker offers: {e:#}") };

        tokio_extras::spawn_fallible(&this, task, err_handler);
    }
}

/// Message used to inform other actors about the maker's latest
/// offers.
pub struct LatestMakerOffers(pub Option<MakerOffers>);

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
