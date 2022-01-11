use daemon::bitmex_price_feed;
use mockall::*;
use std::sync::Arc;
use tokio::sync::Mutex;
use xtra_productivity::xtra_productivity;

pub struct PriceFeedActor {
    pub mock: Arc<Mutex<dyn PriceFeed + Send>>,
}

impl xtra::Actor for PriceFeedActor {}

#[xtra_productivity(message_impl = false)]
impl PriceFeedActor {
    async fn handle(
        &mut self,
        _: bitmex_price_feed::LatestQuote,
    ) -> Option<bitmex_price_feed::Quote> {
        self.mock.lock().await.latest_quote()
    }
}

#[automock]
pub trait PriceFeed {
    fn latest_quote(&mut self) -> Option<bitmex_price_feed::Quote> {
        unreachable!("mockall will reimplement this method")
    }
}
