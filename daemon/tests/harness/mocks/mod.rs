use std::sync::Arc;

use tokio::sync::{Mutex, MutexGuard};

use self::monitor::MonitorActor;
use self::oracle::OracleActor;
use self::wallet::WalletActor;

use super::bdk::{dummy_partially_signed_transaction, dummy_tx_id};

pub mod monitor;
pub mod oracle;
pub mod wallet;

#[derive(Clone)]
pub struct Mocks {
    pub wallet: Arc<Mutex<wallet::MockWallet>>,
    pub monitor: Arc<Mutex<monitor::MockMonitor>>,
    pub oracle: Arc<Mutex<oracle::MockOracle>>,
}

impl Mocks {
    pub async fn wallet(&mut self) -> MutexGuard<'_, wallet::MockWallet> {
        self.wallet.lock().await
    }

    #[allow(dead_code)] // will be used soon
    pub async fn monitor(&mut self) -> MutexGuard<'_, monitor::MockMonitor> {
        self.monitor.lock().await
    }

    pub async fn oracle(&mut self) -> MutexGuard<'_, oracle::MockOracle> {
        self.oracle.lock().await
    }

    // Helper function setting up a "happy path" wallet mock
    pub async fn mock_wallet_sign_and_broadcast(&mut self) {
        let mut seq = mockall::Sequence::new();
        self.wallet()
            .await
            .expect_sign()
            .times(1)
            .returning(|_| Ok(dummy_partially_signed_transaction()))
            .in_sequence(&mut seq);
        self.wallet()
            .await
            .expect_broadcast()
            .times(1)
            .returning(|_| Ok(dummy_tx_id()))
            .in_sequence(&mut seq);
    }

    pub async fn mock_oracle_annoucement(&mut self) {
        self.oracle()
            .await
            .expect_get_announcement()
            .return_const(Some(oracle::dummy_announcement()));
    }

    pub async fn mock_party_params(&mut self) {
        #[allow(clippy::redundant_closure)] // clippy is in the wrong here
        self.wallet()
            .await
            .expect_build_party_params()
            .returning(|msg| wallet::build_party_params(msg));
    }
}

impl Default for Mocks {
    fn default() -> Self {
        Self {
            oracle: Arc::new(Mutex::new(oracle::MockOracle::new())),
            monitor: Arc::new(Mutex::new(monitor::MockMonitor::new())),
            wallet: Arc::new(Mutex::new(wallet::MockWallet::new())),
        }
    }
}

/// Creates actors with embedded mock handlers
pub fn create_actors(mocks: &Mocks) -> (OracleActor, MonitorActor, WalletActor) {
    let oracle = OracleActor {
        mock: mocks.oracle.clone(),
    };
    let monitor = MonitorActor {
        mock: mocks.monitor.clone(),
    };
    let wallet = WalletActor {
        mock: mocks.wallet.clone(),
    };
    (oracle, monitor, wallet)
}
