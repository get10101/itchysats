use crate::db;
use async_trait::async_trait;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

/// Interval at which we delete irrelevant DLC data from the database.
const CULL_INTERVAL: Duration = Duration::from_secs(30 * 60);

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
        self.tasks.add(this.send_interval(CULL_INTERVAL, || Cull));
    }

    async fn stopped(self) -> Self::Stop {}
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: Cull) {
        if let Err(e) = self.db.cull_old_dlcs().await {
            tracing::warn!("Failed to cull old DLC data: {e:#}");
        }
    }
}

struct Cull;
