use crate::db;
use async_trait::async_trait;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

/// Interval at which we move closed CFDs to the correct database
/// table.
const CLOSE_CFDS_INTERVAL: Duration = Duration::from_secs(5 * 60);

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
            .add(this.send_interval(CLOSE_CFDS_INTERVAL, || CloseCfds));
    }

    async fn stopped(self) -> Self::Stop {}
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: CloseCfds) {
        if let Err(e) = self.db.move_to_closed_cfds().await {
            tracing::warn!("Failed to move closed CFDs to corresponding table: {e:#}");
        }
    }
}

struct CloseCfds;
