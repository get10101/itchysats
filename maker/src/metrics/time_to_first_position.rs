use async_trait::async_trait;
use model::Identity;
use sqlite_db;
use time::OffsetDateTime;
use xtra_productivity::xtra_productivity;

#[derive(Clone, Copy)]
pub struct Connected {
    taker_id: Identity,
    timestamp: OffsetDateTime,
}

impl Connected {
    pub fn new(taker_id: Identity) -> Self {
        Self {
            taker_id,
            timestamp: OffsetDateTime::now_utc(),
        }
    }
}

#[derive(Clone, Copy)]
pub struct Position {
    taker_id: Identity,
    timestamp: OffsetDateTime,
}

impl Position {
    pub fn new(taker_id: Identity) -> Self {
        Self {
            taker_id,
            timestamp: OffsetDateTime::now_utc(),
        }
    }
}

pub struct Actor {
    db: sqlite_db::Connection,
}

impl Actor {
    pub fn new(db: sqlite_db::Connection) -> Self {
        Self { db }
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle_taker_connected(
        &mut self,
        Connected {
            taker_id,
            timestamp,
        }: Connected,
    ) {
        if let Err(e) = self.db.try_insert_first_seen(taker_id, timestamp).await {
            tracing::warn!(%taker_id, "Failed to record potential first time taker_id was seen: {e:#}");
        }
    }

    async fn handle_first_position(
        &mut self,
        Position {
            taker_id,
            timestamp,
        }: Position,
    ) {
        if let Err(e) = self.db.try_insert_first_position(taker_id, timestamp).await {
            tracing::warn!(%taker_id, "Failed to record potential first time taker_id opened a position: {e:#}");
        }
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
