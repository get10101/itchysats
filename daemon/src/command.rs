use crate::cfd_actors::load_cfd;
use crate::model::cfd::Cfd;
use crate::model::cfd::Event;
use crate::process_manager;
use crate::OrderId;
use anyhow::Context;
use anyhow::Result;
use xtra::Address;

#[derive(Clone)]
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

    pub async fn execute<T: ExtractEventFromTuple>(
        &self,
        id: OrderId,
        command: impl FnOnce(Cfd) -> Result<T>,
    ) -> Result<T::Rest> {
        let mut connection = self
            .db
            .acquire()
            .await
            .context("Failed to acquire DB connection")?;
        let cfd = load_cfd(id, &mut connection)
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

// TODO: Delete this weird thing once all our commands return only an `Event` and not other stuff as
// well.
pub trait ExtractEventFromTuple {
    type Rest;

    fn extract_event(self) -> (Option<Event>, Self::Rest);
}

impl ExtractEventFromTuple for Option<Event> {
    type Rest = ();

    fn extract_event(self) -> (Option<Event>, Self::Rest) {
        (self, ())
    }
}

impl ExtractEventFromTuple for Event {
    type Rest = ();

    fn extract_event(self) -> (Option<Event>, Self::Rest) {
        (Some(self), ())
    }
}

impl<TOne> ExtractEventFromTuple for (Event, TOne) {
    type Rest = TOne;

    fn extract_event(self) -> (Option<Event>, Self::Rest) {
        (Some(self.0), self.1)
    }
}

impl<TOne, TTwo> ExtractEventFromTuple for (Event, TOne, TTwo) {
    type Rest = (TOne, TTwo);

    fn extract_event(self) -> (Option<Event>, Self::Rest) {
        (Some(self.0), (self.1, self.2))
    }
}

impl<TOne, TTwo, TThree> ExtractEventFromTuple for (Event, TOne, TTwo, TThree) {
    type Rest = (TOne, TTwo, TThree);

    fn extract_event(self) -> (Option<Event>, Self::Rest) {
        (Some(self.0), (self.1, self.2, self.3))
    }
}
