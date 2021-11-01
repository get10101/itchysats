use std::sync::Arc;

use tokio::sync::{Mutex, MutexGuard};

use self::monitor::MonitorActor;
use self::oracle::OracleActor;
use self::wallet::WalletActor;

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
