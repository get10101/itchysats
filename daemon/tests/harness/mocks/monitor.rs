use daemon::monitor;
use daemon::oracle;
use mockall::*;
use std::sync::Arc;
use tokio::sync::Mutex;
use xtra_productivity::xtra_productivity;

/// Test Stub simulating the Monitor actor.
/// Serves as an entrypoint for injected mock handlers.
pub struct MonitorActor {
    pub mock: Arc<Mutex<dyn Monitor + Send>>,
}

impl xtra::Actor for MonitorActor {}
impl Monitor for MonitorActor {}

#[xtra_productivity(message_impl = false)]
impl MonitorActor {
    async fn handle(&mut self, msg: monitor::Sync) {
        self.mock.lock().await.sync(msg)
    }

    async fn handle(&mut self, msg: monitor::StartMonitoring) {
        self.mock.lock().await.start_monitoring(msg)
    }

    async fn handle(&mut self, msg: monitor::CollaborativeSettlement) {
        self.mock.lock().await.collaborative_settlement(msg)
    }

    async fn handle(&mut self, msg: oracle::Attestation) {
        self.mock.lock().await.oracle_attestation(msg);
    }
}

#[automock]
pub trait Monitor {
    fn sync(&mut self, _msg: monitor::Sync) {
        unreachable!("mockall will reimplement this method")
    }

    fn start_monitoring(&mut self, _msg: monitor::StartMonitoring) {
        unreachable!("mockall will reimplement this method")
    }

    fn collaborative_settlement(&mut self, _msg: monitor::CollaborativeSettlement) {
        unreachable!("mockall will reimplement this method")
    }

    fn oracle_attestation(&mut self, _msg: oracle::Attestation) {
        unreachable!("mockall will reimplement this method")
    }
}
