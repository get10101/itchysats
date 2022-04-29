use crate::db;
use async_trait::async_trait;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

/// Interval at which we archive closed CFDs in the correct database
/// table.
const ARCHIVE_CFDS_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub struct Actor {
    db: db::Connection,
    tasks: Tasks,
}

impl Actor {
    pub fn new(db: db::Connection) -> Self {
        Self {
            db,
            tasks: Tasks::default(),
        }
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");
        self.tasks
            .add(this.send_interval(ARCHIVE_CFDS_INTERVAL, || ArchiveCfds));
    }

    async fn stopped(self) -> Self::Stop {}
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: ArchiveCfds) {
        if let Err(e) = self.db.move_to_closed_cfds().await {
            tracing::warn!("Failed to archive closed CFDs to corresponding table: {e:#}");
        }
    }
}

struct ArchiveCfds;
