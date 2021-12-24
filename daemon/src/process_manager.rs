use crate::db::append_event;
use crate::model::cfd;
use crate::model::cfd::Role;
use crate::projection;
use anyhow::Result;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    db: sqlx::SqlitePool,
    _role: Role,
    cfds_changed: Box<dyn MessageChannel<projection::CfdsChanged>>,
}

pub struct Event(cfd::Event);

impl Event {
    pub fn new(event: cfd::Event) -> Self {
        Self(event)
    }
}

impl Actor {
    pub fn new(
        db: sqlx::SqlitePool,
        role: Role,
        cfds_changed: &(impl MessageChannel<projection::CfdsChanged> + 'static),
    ) -> Self {
        Self {
            db,
            _role: role,
            cfds_changed: cfds_changed.clone_channel(),
        }
    }
}

#[xtra_productivity]
impl Actor {
    fn handle(&mut self, msg: Event) -> Result<()> {
        let event = msg.0;

        // 1. Safe in DB
        let mut conn = self.db.acquire().await?;
        append_event(event.clone(), &mut conn).await?;

        // TODO: 2. Post-process event by sending out messages

        // 3. Update UI
        self.cfds_changed.send(projection::CfdsChanged).await?;

        Ok(())
    }
}

impl xtra::Actor for Actor {}
