use crate::harness::maia::OliviaData;
use daemon::oracle;
use mockall::*;
use std::sync::Arc;
use tokio::sync::Mutex;
use xtra_productivity::xtra_productivity;

/// Test Stub simulating the Oracle actor.
/// Serves as an entrypoint for injected mock handlers.
pub struct OracleActor {
    pub mock: Arc<Mutex<dyn Oracle + Send>>,
}

impl xtra::Actor for OracleActor {}
impl Oracle for OracleActor {}

#[xtra_productivity(message_impl = false)]
impl OracleActor {
    async fn handle(
        &mut self,
        msg: oracle::GetAnnouncement,
    ) -> Result<oracle::Announcement, oracle::NoAnnouncement> {
        self.mock.lock().await.get_announcement(msg)
    }

    async fn handle(&mut self, msg: oracle::MonitorAttestation) {
        self.mock.lock().await.monitor_attestation(msg)
    }

    async fn handle(&mut self, msg: oracle::Sync) {
        self.mock.lock().await.sync(msg)
    }
}

#[automock]
pub trait Oracle {
    fn get_announcement(
        &mut self,
        _msg: oracle::GetAnnouncement,
    ) -> Result<oracle::Announcement, oracle::NoAnnouncement> {
        unreachable!("mockall will reimplement this method")
    }

    fn monitor_attestation(&mut self, _msg: oracle::MonitorAttestation) {
        unreachable!("mockall will reimplement this method")
    }

    fn sync(&mut self, _msg: oracle::Sync) {
        unreachable!("mockall will reimplement this method")
    }
}

pub fn dummy_announcement() -> oracle::Announcement {
    OliviaData::example_0().announcement()
}
