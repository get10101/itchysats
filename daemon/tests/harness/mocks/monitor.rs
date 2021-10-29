use daemon::{monitor, oracle};
use xtra_productivity::xtra_productivity;

/// Test Stub simulating the Monitor actor
pub struct Monitor;
impl xtra::Actor for Monitor {}

#[xtra_productivity(message_impl = false)]
impl Monitor {
    async fn handle(&mut self, _msg: monitor::Sync) {}

    async fn handle(&mut self, _msg: monitor::StartMonitoring) {
        todo!("stub this if needed")
    }

    async fn handle(&mut self, _msg: monitor::CollaborativeSettlement) {
        todo!("stub this if needed")
    }

    async fn handle(&mut self, _msg: oracle::Attestation) {
        todo!("stub this if needed")
    }
}

impl Default for Monitor {
    fn default() -> Self {
        Monitor
    }
}
