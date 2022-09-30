use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;
use xtra_bitmex_price_feed::LatestQuotes;
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
    type Stop = xtra_bitmex_price_feed::Error;

    async fn stopped(self) -> Self::Stop {
        xtra_bitmex_price_feed::Error::Unspecified
    }
}

#[xtra_productivity]
impl PriceFeedActor {
    async fn handle(&mut self, _: xtra_bitmex_price_feed::GetLatestQuotes) -> LatestQuotes {
        self.mock.lock().await.latest_quotes()
    }
}

#[derive(Default, Clone)]
pub struct MockPriceFeed {
    latest_quotes: LatestQuotes,
}

impl MockPriceFeed {
    pub fn latest_quotes(&self) -> LatestQuotes {
        self.latest_quotes.clone()
    }

    pub fn set_latest_quotes(&mut self, new_quote: LatestQuotes) {
        self.latest_quotes = new_quote;
    }
}
