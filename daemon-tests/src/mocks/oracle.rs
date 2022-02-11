use crate::maia::OliviaData;
use async_trait::async_trait;
use daemon::oracle;
use mockall::*;
use model::olivia;
use model::olivia::BitMexPriceEventId;
use std::sync::Arc;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use xtra_productivity::xtra_productivity;

/// Test Stub simulating the Oracle actor.
/// Serves as an entrypoint for injected mock handlers.
pub struct OracleActor {
    mock: Arc<Mutex<MockOracle>>,
}

impl OracleActor {
    pub fn new() -> (Self, Arc<Mutex<MockOracle>>) {
        let mock = Arc::new(Mutex::new(MockOracle::new()));
        let actor = Self { mock: mock.clone() };

        (actor, mock)
    }
}

#[async_trait]
impl xtra::Actor for OracleActor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
impl Oracle for OracleActor {}

#[xtra_productivity(message_impl = false)]
impl OracleActor {
    async fn handle(
        &mut self,
        msg: oracle::GetAnnouncement,
    ) -> Result<olivia::Announcement, oracle::NoAnnouncement> {
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
    ) -> Result<olivia::Announcement, oracle::NoAnnouncement> {
        unreachable!("mockall will reimplement this method")
    }

    fn monitor_attestation(&mut self, _msg: oracle::MonitorAttestation) {
        unreachable!("mockall will reimplement this method")
    }

    fn sync(&mut self, _msg: oracle::Sync) {
        unreachable!("mockall will reimplement this method")
    }
}

/// We do *not* depend on the current time in our tests, the valid combination of
/// announcement/attestation is hard-coded in OliviaData struct (along with event id's).
/// Therefore, an attestation based on current utc time will always be wrong.
pub fn dummy_wrong_attestation() -> oracle::Attestation {
    let olivia::Attestation {
        id: _,
        price,
        scalars,
    } = OliviaData::example_0().attestation().into_inner();

    oracle::Attestation::new(olivia::Attestation {
        id: BitMexPriceEventId::with_20_digits(OffsetDateTime::now_utc()),
        price,
        scalars,
    })
}
