use super::maia::OliviaData;
use crate::mocks::price_feed::MockPriceFeed;
use crate::mocks::wallet::MockWallet;
use model::olivia;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::MutexGuard;

pub mod monitor;
pub mod oracle;
pub mod price_feed;
pub mod wallet;

#[derive(Clone)]
pub struct Mocks {
    wallet: Arc<Mutex<wallet::MockWallet>>,
    monitor: Arc<Mutex<monitor::MockMonitor>>,
    oracle: Arc<Mutex<oracle::MockOracle>>,
    price_feed: Arc<Mutex<price_feed::MockPriceFeed>>,
}

impl Mocks {
    pub fn new(
        wallet: Arc<Mutex<MockWallet>>,
        price_feed: Arc<Mutex<MockPriceFeed>>,
        monitor: Arc<Mutex<monitor::MockMonitor>>,
        oracle: Arc<Mutex<oracle::MockOracle>>,
    ) -> Mocks {
        Self {
            wallet,
            monitor,
            oracle,
            price_feed,
        }
    }

    pub async fn wallet(&mut self) -> MutexGuard<'_, wallet::MockWallet> {
        self.wallet.lock().await
    }

    pub async fn oracle(&mut self) -> MutexGuard<'_, oracle::MockOracle> {
        self.oracle.lock().await
    }

    pub async fn price_feed(&mut self) -> MutexGuard<'_, price_feed::MockPriceFeed> {
        self.price_feed.lock().await
    }

    pub async fn monitor(&mut self) -> MutexGuard<'_, monitor::MockMonitor> {
        self.monitor.lock().await
    }

    // Helper function setting up a "happy path" wallet mock
    pub async fn mock_wallet_sign_and_broadcast(&mut self) {
        self.wallet()
            .await
            .expect_sign()
            .returning(|sign_msg| Ok(sign_msg.psbt));
    }

    pub async fn mock_oracle_announcement(&mut self) {
        self.mock_oracle_announcement_with(OliviaData::example_0().announcement())
            .await;
    }

    pub async fn mock_oracle_announcement_with(&mut self, announcement: olivia::Announcement) {
        self.oracle().await.set_announcement(announcement);
    }

    pub async fn mock_party_params(&mut self) {
        #[allow(clippy::redundant_closure)] // clippy is in the wrong here
        self.wallet()
            .await
            .expect_build_party_params()
            .returning(|msg| wallet::build_party_params(msg));
    }

    pub async fn mock_latest_quote(&mut self, latest_quote: Option<xtra_bitmex_price_feed::Quote>) {
        self.price_feed().await.set_latest_quote(latest_quote);
    }
}
