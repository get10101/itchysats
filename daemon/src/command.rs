use crate::cfd_actors::load_cfd;
use crate::model::cfd::Cfd;
use crate::model::cfd::Event;
use crate::process_manager;
use crate::OrderId;
use anyhow::Context;
use anyhow::Result;
use xtra::Address;

pub struct Executor {
    db: sqlx::SqlitePool,
    process_manager: Address<process_manager::Actor>,
}

impl Executor {
    pub fn new(db: sqlx::SqlitePool, process_manager: Address<process_manager::Actor>) -> Self {
        Self {
            db,
            process_manager,
        }
    }

    pub async fn execute(
        &self,
        id: OrderId,
        command: impl FnOnce(Cfd) -> Result<Event>,
    ) -> Result<()> {
        let mut connection = self
            .db
            .acquire()
            .await
            .context("Failed to acquire DB connection")?;
        let cfd = load_cfd(id, &mut connection)
            .await
            .context("Failed to load CFD")?;

        let event = command(cfd).context("Failed to execute command on CFD")?;

        self.process_manager
            .send(process_manager::Event::new(event))
            .await
            .context("ProcessManager is disconnected")?
            .context("Failed to process new domain event")?;

        Ok(())
    }
}
