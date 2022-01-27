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
    async fn handle(&mut self, _: monitor::Sync) {}

    async fn handle(&mut self, _: monitor::StartMonitoring) {}

    async fn handle(&mut self, _: monitor::CollaborativeSettlement) {}

    async fn handle(&mut self, _: oracle::Attestation) {}

    async fn handle(&mut self, _: monitor::TryBroadcastTransaction) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
pub struct MockMonitor {}

impl MockMonitor {}
