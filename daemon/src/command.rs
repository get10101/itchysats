use crate::process_manager;
use crate::OrderId;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use model::Cfd;
use model::ExtractEventFromTuple;
use sqlite_db;
use std::fmt;
use std::fmt::Debug;
use xtra::Address;

#[derive(Clone)]
pub struct Executor {
    db: sqlite_db::Connection,
    process_manager: Address<process_manager::Actor>,
}

impl Debug for Executor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Executor")
            .field("process_manager", &self.process_manager)
            .finish()
    }
}

impl Executor {
    pub fn new(
        db: sqlite_db::Connection,
        process_manager: Address<process_manager::Actor>,
    ) -> Self {
        Self {
            db,
            process_manager,
        }
    }

    pub async fn execute<T: ExtractEventFromTuple>(
        &self,
        id: OrderId,
        command: impl FnOnce(Cfd) -> Result<T>,
    ) -> Result<T::Rest> {
        let cfd = self
            .db
            .load_open_cfd(id, ())
            .await
            .context("Failed to load CFD")?;

        let return_val = command(cfd).context("Failed to execute command on CFD")?;

        let (event, rest) = return_val.extract_event();

        if let Some(event) = event {
            self.process_manager
                .send(process_manager::Event::new(event))
                .await
                .context("ProcessManager is disconnected")?
                .context("Failed to process new domain event")?;
        }

        Ok(rest)
    }
}

#[async_trait]
impl model::ExecuteOnCfd for Executor {
    async fn execute<T>(
        &self,
        id: OrderId,
        command: impl FnOnce(Cfd) -> Result<T> + Send,
    ) -> Result<T::Rest>
    where
        T: ExtractEventFromTuple + Send,
        T::Rest: Send,
    {
        self.execute(id, command).await
    }
}
