use daemon::model::BitMexPriceEventId;
use daemon::oracle;
use time::OffsetDateTime;
use xtra_productivity::xtra_productivity;

pub struct Oracle {
    announcement: Option<oracle::Announcement>,
}

impl Oracle {
    pub fn with_dummy_announcement(
        mut self,
        dummy_announcement: cfd_protocol::Announcement,
    ) -> Self {
        self.announcement = Some(oracle::Announcement {
            id: BitMexPriceEventId::new(OffsetDateTime::UNIX_EPOCH, 0),
            expected_outcome_time: OffsetDateTime::now_utc(),
            nonce_pks: dummy_announcement.nonce_pks,
        });

        self
    }
}

impl xtra::Actor for Oracle {}

#[xtra_productivity(message_impl = false)]
impl Oracle {
    async fn handle_get_announcement(
        &mut self,
        _msg: oracle::GetAnnouncement,
    ) -> Option<oracle::Announcement> {
        self.announcement.clone()
    }

    async fn handle(&mut self, _msg: oracle::MonitorAttestation) {}

    async fn handle(&mut self, _msg: oracle::Sync) {}
}

impl Default for Oracle {
    fn default() -> Self {
        Oracle { announcement: None }
    }
}
