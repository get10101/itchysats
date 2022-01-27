use self::monitor::MonitorActor;
use self::oracle::OracleActor;
use self::wallet::WalletActor;
use super::maia::OliviaData;
use crate::mocks::price_feed::PriceFeedActor;
use daemon::bitmex_price_feed;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::MutexGuard;

pub mod monitor;
pub mod oracle;
pub mod price_feed;
pub mod wallet;

#[derive(Clone)]
pub struct Mocks {
    pub wallet: Arc<Mutex<wallet::MockWallet>>,
    pub monitor: Arc<Mutex<monitor::MockMonitor>>,
    pub oracle: Arc<Mutex<oracle::MockOracle>>,
    pub price_feed: Arc<Mutex<price_feed::MockPriceFeed>>,
}

impl Mocks {
    pub fn new() -> Self {
        Self {
            oracle: Arc::new(Mutex::new(oracle::MockOracle::new())),
            monitor: Arc::new(Mutex::new(monitor::MockMonitor::default())),
            wallet: Arc::new(Mutex::new(wallet::MockWallet::new())),
            price_feed: Arc::new(Mutex::new(price_feed::MockPriceFeed::default())),
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

    pub async fn mock_sync_handlers(&mut self) {
        self.oracle().await.expect_sync().return_const(());
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

    pub async fn mock_oracle_announcement_with(
        &mut self,
        announcement: daemon::oracle::Announcement,
    ) {
        self.oracle()
            .await
            .expect_get_announcement()
            .return_const(Ok(announcement));
    }

    pub async fn mock_oracle_monitor_attestation(&mut self) {
        self.oracle()
            .await
            .expect_monitor_attestation()
            .return_const(());
    }

    pub async fn mock_party_params(&mut self) {
        #[allow(clippy::redundant_closure)] // clippy is in the wrong here
        self.wallet()
            .await
            .expect_build_party_params()
            .returning(|msg| wallet::build_party_params(msg));
    }

    pub async fn mock_latest_quote(&mut self, latest_quote: Option<bitmex_price_feed::Quote>) {
        self.price_feed().await.set_latest_quote(latest_quote);
    }
}

/// Creates actors with embedded mock handlers
pub fn create_actors(mocks: &Mocks) -> (OracleActor, MonitorActor, WalletActor, PriceFeedActor) {
    let oracle = OracleActor::new(mocks.oracle.clone());
    let monitor = MonitorActor::new(mocks.monitor.clone());
    let wallet = WalletActor::new(mocks.wallet.clone());
    let price_feed = PriceFeedActor::new(mocks.price_feed.clone());

    (oracle, monitor, wallet, price_feed)
}
