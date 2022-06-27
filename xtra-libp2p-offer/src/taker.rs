use crate::protocol;
use async_trait::async_trait;
use model::MakerOffers;
use xtra::prelude::MessageChannel;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor as _;
use xtra_libp2p::NewInboundSubstream;
use xtra_productivity::xtra_productivity;
use xtras::spawner;
use xtras::spawner::SpawnFallible;
use xtras::SendAsyncSafe;

pub struct Actor {
    maker_offers: Box<dyn MessageChannel<LatestMakerOffers>>,
    spawner: xtra::Address<spawner::Actor>,
}

impl Actor {
    pub fn new(maker_offers: &(impl MessageChannel<LatestMakerOffers> + 'static)) -> Self {
        let spawner = spawner::Actor::new().create(None).spawn_global();

        Self {
            maker_offers: maker_offers.clone_channel(),
            spawner,
        }
    }
}

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle(&mut self, msg: NewInboundSubstream) {
        let NewInboundSubstream { peer, stream } = msg;
        let maker_offers = self.maker_offers.clone_channel();

        let task = async move {
            let offers = protocol::recv(stream).await?;

            tracing::debug!(%peer, ?offers, "Received offers");

            maker_offers.send(LatestMakerOffers(offers)).await?;

            anyhow::Ok(())
        };

        let err_handler =
            move |e| async move { tracing::debug!(%peer, "Failed to process maker offers: {e:#}") };

        if let Err(e) = self
            .spawner
            .send_async_safe(SpawnFallible::new(task, err_handler))
            .await
        {
            tracing::warn!("Failed to spawn task to process new offers: {e:#}");
        };
    }
}

/// Message used to inform other actors about the maker's latest
/// offers.
pub struct LatestMakerOffers(pub Option<MakerOffers>);

impl xtra::Message for LatestMakerOffers {
    type Result = ();
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
