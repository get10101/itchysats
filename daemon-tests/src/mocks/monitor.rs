use anyhow::Result;
use daemon::monitor;
use daemon::oracle;
use std::sync::Arc;
use tokio::sync::Mutex;
use xtra_productivity::xtra_productivity;

/// Test Stub simulating the Monitor actor.
/// Serves as an entrypoint for injected mock handlers.
pub struct MonitorActor {
    pub mock: Arc<Mutex<MockMonitor>>,
}

impl xtra::Actor for MonitorActor {}

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

    async fn handle(&mut self, msg: monitor::TryBroadcastTransaction) -> Result<()> {
        self.mock.lock().await.broadcast(msg)
    }
}

#[derive(Default)]
pub struct MockMonitor {}

impl MockMonitor {
    fn sync(&mut self, _msg: monitor::Sync) {}

    fn start_monitoring(&mut self, _msg: monitor::StartMonitoring) {}

    fn collaborative_settlement(&mut self, _msg: monitor::CollaborativeSettlement) {}

    fn oracle_attestation(&mut self, _msg: oracle::Attestation) {}

    fn broadcast(&mut self, _msg: monitor::TryBroadcastTransaction) -> Result<()> {
        Ok(())
    }
}
