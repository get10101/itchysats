use async_trait::async_trait;
use daemon::bitmex_price_feed;
use std::sync::Arc;
use tokio::sync::Mutex;
use xtra_productivity::xtra_productivity;

#[derive(Clone)]
pub struct PriceFeedActor {
    mock: Arc<Mutex<MockPriceFeed>>,
}

impl PriceFeedActor {
    pub fn new() -> (PriceFeedActor, Arc<Mutex<MockPriceFeed>>) {
        let mock = Arc::new(Mutex::new(MockPriceFeed::default()));
        let actor = Self { mock: mock.clone() };

        (actor, mock)
    }
}

#[async_trait]
impl xtra::Actor for PriceFeedActor {
    type Stop = bitmex_price_feed::Error;

    async fn stopped(self) -> Self::Stop {
        bitmex_price_feed::Error::Unspecified
    }
}

#[xtra_productivity(message_impl = false)]
impl PriceFeedActor {
    async fn handle(
        &mut self,
        _: bitmex_price_feed::LatestQuote,
    ) -> Option<bitmex_price_feed::Quote> {
        self.mock.lock().await.latest_quote()
    }
}

#[derive(Default)]
pub struct MockPriceFeed {
    latest_quote: Option<bitmex_price_feed::Quote>,
}

impl MockPriceFeed {
    pub fn latest_quote(&self) -> Option<bitmex_price_feed::Quote> {
        self.latest_quote
    }

    pub fn set_latest_quote(&mut self, new_quote: Option<bitmex_price_feed::Quote>) {
        self.latest_quote = new_quote;
    }
}
