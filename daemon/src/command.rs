use crate::db;
use crate::process_manager;
use crate::OrderId;
use anyhow::Context;
use anyhow::Result;
use model::Cfd;
use model::CfdEvent;
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

        let cfd = db::load_cfd(&mut connection, id, ())
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

impl db::CfdAggregate for Cfd {
    type CtorArgs = ();

    fn new(
        _: Self::CtorArgs,
        db::Cfd {
            id,
            position,
            initial_price,
            taker_leverage: leverage,
            settlement_interval,
            counterparty_network_identity,
            role,
            quantity_usd,
            opening_fee,
            initial_funding_rate,
            initial_tx_fee_rate,
        }: db::Cfd,
    ) -> Self {
        Cfd::new(
            id,
            position,
            initial_price,
            leverage,
            settlement_interval,
            role,
            quantity_usd,
            counterparty_network_identity,
            opening_fee,
            initial_funding_rate,
            initial_tx_fee_rate,
        )
    }

    fn apply(self, event: CfdEvent) -> Self {
        self.apply(event)
    }

    fn version(&self) -> u32 {
        self.version()
    }
}

// TODO: Delete this weird thing once all our commands return only an `Event` and not other stuff as
// well.
pub trait ExtractEventFromTuple {
    type Rest;

    fn extract_event(self) -> (Option<CfdEvent>, Self::Rest);
}

impl ExtractEventFromTuple for Option<CfdEvent> {
    type Rest = ();

    fn extract_event(self) -> (Option<CfdEvent>, Self::Rest) {
        (self, ())
    }
}

impl ExtractEventFromTuple for CfdEvent {
    type Rest = ();

    fn extract_event(self) -> (Option<CfdEvent>, Self::Rest) {
        (Some(self), ())
    }
}

impl<TOne> ExtractEventFromTuple for (CfdEvent, TOne) {
    type Rest = TOne;

    fn extract_event(self) -> (Option<CfdEvent>, Self::Rest) {
        (Some(self.0), self.1)
    }
}

impl<TOne, TTwo> ExtractEventFromTuple for (CfdEvent, TOne, TTwo) {
    type Rest = (TOne, TTwo);

    fn extract_event(self) -> (Option<CfdEvent>, Self::Rest) {
        (Some(self.0), (self.1, self.2))
    }
}

impl<TOne, TTwo, TThree> ExtractEventFromTuple for (CfdEvent, TOne, TTwo, TThree) {
    type Rest = (TOne, TTwo, TThree);

    fn extract_event(self) -> (Option<CfdEvent>, Self::Rest) {
        (Some(self.0), (self.1, self.2, self.3))
    }
}

impl<TOne, TTwo, TThree, TFour> ExtractEventFromTuple for (CfdEvent, TOne, TTwo, TThree, TFour) {
    type Rest = (TOne, TTwo, TThree, TFour);

    fn extract_event(self) -> (Option<CfdEvent>, Self::Rest) {
        (Some(self.0), (self.1, self.2, self.3, self.4))
    }
}

impl<TOne, TTwo, TThree, TFour, TFive> ExtractEventFromTuple
    for (CfdEvent, TOne, TTwo, TThree, TFour, TFive)
{
    type Rest = (TOne, TTwo, TThree, TFour, TFive);

    fn extract_event(self) -> (Option<CfdEvent>, Self::Rest) {
        (Some(self.0), (self.1, self.2, self.3, self.4, self.5))
    }
}
