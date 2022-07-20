use async_trait::async_trait;
use sqlite_db;
use std::time::Duration;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

/// Interval at which we archive failed CFDs in the correct database
/// table.
const ARCHIVE_CFDS_INTERVAL: Duration = Duration::from_secs(30 * 60);

pub struct Actor {
    db: sqlite_db::Connection,
}

impl Actor {
    pub fn new(db: sqlite_db::Connection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");
        tokio_extras::spawn(
            &this.clone(),
            this.send_interval(
                ARCHIVE_CFDS_INTERVAL,
                || ArchiveCfds,
                xtras::IncludeSpan::Always,
            ),
        );
    }

    async fn stopped(self) -> Self::Stop {}
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: ArchiveCfds) {
        if let Err(e) = self.db.move_to_failed_cfds().await {
            tracing::warn!("Failed to archive failed CFDs to corresponding table: {e:#}");
        }
    }
}

struct ArchiveCfds;
