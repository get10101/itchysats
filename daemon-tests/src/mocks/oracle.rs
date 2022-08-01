use crate::maia::OliviaData;
use async_trait::async_trait;
use daemon::command;
use daemon::oracle;
use model::olivia;
use model::olivia::BitMexPriceEventId;
use model::OrderId;
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
    pub fn new(executor: command::Executor) -> (Self, Arc<Mutex<MockOracle>>) {
        let mock = Arc::new(Mutex::new(MockOracle::new(executor)));
        let actor = Self { mock: mock.clone() };

        (actor, mock)
    }
}

#[async_trait]
impl xtra::Actor for OracleActor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

#[xtra_productivity]
impl OracleActor {
    /// Handle a request to get `Announcement`s.
    ///
    /// We _ignore_ the message that is supposed to indicate which `Announcement`s the sender is
    /// requesting. Instead, we return whatever `Announcement`s we have mocked beforehand.
    async fn handle(
        &mut self,
        _: oracle::GetAnnouncements,
    ) -> Result<Vec<olivia::Announcement>, oracle::NoAnnouncement> {
        Ok(self
            .mock
            .lock()
            .await
            .announcements
            .clone()
            .expect("To have mocked announcements"))
    }

    async fn handle(&mut self, _msg: oracle::MonitorAttestations) {}

    async fn handle(&mut self, _msg: oracle::SyncAnnouncements) {}

    async fn handle(&mut self, _msg: oracle::SyncAttestations) {}
}

pub struct MockOracle {
    executor: command::Executor,
    announcements: Option<Vec<olivia::Announcement>>,
}

impl MockOracle {
    fn new(executor: command::Executor) -> Self {
        Self {
            executor,
            announcements: None,
        }
    }

    pub async fn simulate_attestation(&mut self, id: OrderId, attestation: &oracle::Attestation) {
        self.executor
            .execute(id, |cfd| cfd.decrypt_cet(attestation.as_inner()))
            .await
            .unwrap();
    }

    pub fn set_announcements(&mut self, announcements: Vec<olivia::Announcement>) {
        self.announcements = Some(announcements);
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
    } = OliviaData::example_0().attestations()[0]
        .clone()
        .into_inner();

    oracle::Attestation::new(olivia::Attestation {
        id: BitMexPriceEventId::with_20_digits(OffsetDateTime::now_utc()),
        price,
        scalars,
    })
}
