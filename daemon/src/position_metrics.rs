use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use model::CfdEvent;
use model::EventKind;
use model::FailedCfd;
use model::FailedKind;
use model::OrderId;
use model::Position;
use model::Usd;
use rust_decimal::Decimal;
use sqlite_db;
use std::collections::HashMap;
use xtra_productivity::xtra_productivity;
use xtras::SendAsyncSafe;

pub struct Actor {
    db: sqlite_db::Connection,
    state: State,
}

/// Internal struct to keep state in one place
struct State {
    cfds: Option<HashMap<OrderId, Cfd>>,
}

impl Actor {
    pub fn new(db: sqlite_db::Connection) -> Self {
        Self {
            db,
            state: State::new(),
        }
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");
        this.send_async_safe(Initialize)
            .await
            .expect("we just started");
    }

    async fn stopped(self) -> Self::Stop {}
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: Initialize) {
        let mut stream = self.db.load_all_cfds::<Cfd>(());

        let mut cfds = HashMap::new();
        while let Some(cfd) = stream.next().await {
            let cfd = match cfd {
                Ok(cfd) => cfd,
                Err(e) => {
                    tracing::error!("Failed to rehydrate CFD: {e:#}");
                    continue;
                }
            };
            cfds.insert(cfd.id, cfd);
        }

        self.state.cfds = Some(cfds);
        metrics::update_position_metrics(
            self.state.cfds.clone().expect("We've initialized it above"),
        );
    }

    async fn handle(&mut self, msg: CfdChanged) {
        if let Err(e) = self.state.update_cfd(self.db.clone(), msg.0).await {
            tracing::error!("Failed to rehydrate CFD: {e:#}");
            return;
        };

        metrics::update_position_metrics(
            self.state
                .cfds
                .clone()
                .expect("updating metrics failed. Internal list has not been initialized yet"),
        );
    }
}

impl State {
    fn new() -> Self {
        Self { cfds: None }
    }

    async fn update_cfd(&mut self, db: sqlite_db::Connection, id: OrderId) -> Result<()> {
        let cfd = db.load_open_cfd(id, ()).await?;

        let cfds = self
            .cfds
            .as_mut()
            .context("CFD list has not been initialized yet")?;

        cfds.insert(id, cfd);

        Ok(())
    }
}

#[derive(Debug)]
struct Initialize;

/// Indicates that the CFD with the given order ID changed.
#[derive(Clone, Copy)]
pub struct CfdChanged(pub OrderId);

/// Read-model of the CFD for the position metrics actor.
#[derive(Clone, Copy)]
pub struct Cfd {
    id: OrderId,
    position: Position,
    quantity_usd: Usd,

    state: AggregatedState,

    version: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AggregatedState {
    /// Used when the CFD is new or in contract setup
    New,
    Open,
    Closed,
    Failed,
    Refunded,
    Rejected,
}

impl sqlite_db::CfdAggregate for Cfd {
    type CtorArgs = ();

    fn new(_: Self::CtorArgs, cfd: sqlite_db::Cfd) -> Self {
        Self {
            id: cfd.id,
            position: cfd.position,
            quantity_usd: cfd.quantity_usd,
            state: AggregatedState::New,
            version: 0,
        }
    }

    fn apply(self, event: CfdEvent) -> Self {
        self.apply(event)
    }

    fn version(&self) -> u32 {
        self.version
    }
}

impl Cfd {
    fn apply(mut self, event: CfdEvent) -> Self {
        self.version += 1;
        use EventKind::*;
        match event.event {
            ContractSetupStarted => Self {
                state: AggregatedState::New,
                ..self
            },
            ContractSetupCompleted { .. } | LockConfirmed => Self {
                state: AggregatedState::Open,
                ..self
            },
            ContractSetupFailed => Self {
                state: AggregatedState::Failed,
                ..self
            },
            OfferRejected => Self {
                state: AggregatedState::Rejected,
                ..self
            },
            RolloverStarted
            | RolloverAccepted
            | RolloverRejected
            | RolloverCompleted { .. }
            | RolloverFailed => Self {
                // should still be open
                ..self
            },
            CollaborativeSettlementStarted { .. }
            | CollaborativeSettlementProposalAccepted
            | CollaborativeSettlementRejected
            | CollaborativeSettlementFailed => Self {
                // should still be open
                ..self
            },
            CollaborativeSettlementCompleted { .. } => Self {
                state: AggregatedState::Closed,
                ..self
            },
            ManualCommit { .. } | CommitConfirmed => Self {
                // we don't know yet if the position will be closed immediately (e.g. through
                // punishing) or a bit later after the oracle has attested to the price
                ..self
            },
            CetConfirmed => Self {
                state: AggregatedState::Closed,
                ..self
            },
            RefundConfirmed => Self {
                state: AggregatedState::Refunded,
                ..self
            },
            RevokeConfirmed => Self {
                // the other party was punished, we are done here!
                state: AggregatedState::Closed,
                ..self
            },
            CollaborativeSettlementConfirmed => Self {
                state: AggregatedState::Closed,
                ..self
            },
            LockConfirmedAfterFinality => Self {
                // This event is only appended if lock confirmation happens after we spent from lock
                // on chain. This is for special case where collaborative settlement is triggered
                // before lock is confirmed. In such a case the CFD is closed
                state: AggregatedState::Closed,
                ..self
            },
            CetTimelockExpiredPriorOracleAttestation
            | CetTimelockExpiredPostOracleAttestation { .. } => Self {
                state: AggregatedState::Closed,
                ..self
            },
            RefundTimelockExpired { .. } => Self {
                // a rollover with an expired timelock should be rejected for settlement and
                // rollover, hence, this is closed
                state: AggregatedState::Closed,
                ..self
            },
            OracleAttestedPriorCetTimelock { .. } | OracleAttestedPostCetTimelock { .. } => Self {
                // we know the closing price already and can assume that the cfd will be closed
                // accordingly
                state: AggregatedState::Closed,
                ..self
            },
        }
    }
}

impl sqlite_db::ClosedCfdAggregate for Cfd {
    fn new_closed(_: Self::CtorArgs, closed_cfd: sqlite_db::ClosedCfd) -> Self {
        let sqlite_db::ClosedCfd {
            id,
            position,
            n_contracts,
            settlement,
            ..
        } = closed_cfd;

        let quantity_usd = Usd::new(Decimal::from(u64::from(n_contracts)));

        let state = match settlement {
            sqlite_db::Settlement::Collaborative { .. } | sqlite_db::Settlement::Cet { .. } => {
                AggregatedState::Closed
            }
            sqlite_db::Settlement::Refund { .. } => AggregatedState::Refunded,
        };

        Self {
            id,
            position,
            quantity_usd,
            state,
            version: 0,
        }
    }
}

impl sqlite_db::FailedCfdAggregate for Cfd {
    fn new_failed(_: Self::CtorArgs, cfd: FailedCfd) -> Self {
        let FailedCfd {
            id,
            position,
            n_contracts,
            kind,
            ..
        } = cfd;

        let quantity_usd = Usd::new(Decimal::from(u64::from(n_contracts)));

        let state = match kind {
            FailedKind::OfferRejected => AggregatedState::Rejected,
            FailedKind::ContractSetupFailed => AggregatedState::Failed,
        };

        Self {
            id,
            position,
            quantity_usd,
            state,
            version: 0,
        }
    }
}

mod metrics {
    use crate::position_metrics::AggregatedState;
    use crate::position_metrics::Cfd;
    use model::OrderId;
    use model::Position;
    use model::Usd;
    use rust_decimal::prelude::ToPrimitive;
    use std::collections::HashMap;

    const POSITION_LABEL: &str = "position";
    const POSITION_LONG_LABEL: &str = "long";
    const POSITION_SHORT_LABEL: &str = "short";

    const STATUS_LABEL: &str = "status";
    const STATUS_NEW_LABEL: &str = "new";
    const STATUS_OPEN_LABEL: &str = "open";
    const STATUS_CLOSED_LABEL: &str = "closed";
    const STATUS_FAILED_LABEL: &str = "failed";
    const STATUS_REJECTED_LABEL: &str = "rejected";
    const STATUS_REFUNDED_LABEL: &str = "refunded";

    static POSITION_QUANTITY_GAUGE: conquer_once::Lazy<prometheus::GaugeVec> =
        conquer_once::Lazy::new(|| {
            prometheus::register_gauge_vec!(
                "positions_quantities",
                "Total quantity of positions on ItchySats.",
                &[POSITION_LABEL, STATUS_LABEL]
            )
            .unwrap()
        });

    static POSITION_AMOUNT_GAUGE: conquer_once::Lazy<prometheus::IntGaugeVec> =
        conquer_once::Lazy::new(|| {
            prometheus::register_int_gauge_vec!(
                "positions_number_total",
                "Total number of positions on ItchySats.",
                &[POSITION_LABEL, STATUS_LABEL]
            )
            .unwrap()
        });

    pub fn update_position_metrics(cfds: HashMap<OrderId, Cfd>) {
        let cfds = cfds.into_iter().map(|(_, cfd)| cfd).collect::<Vec<_>>();

        set_position_metrics(
            cfds.iter().filter(|cfd| cfd.state == AggregatedState::New),
            STATUS_NEW_LABEL,
        );
        set_position_metrics(
            cfds.iter().filter(|cfd| cfd.state == AggregatedState::Open),
            STATUS_OPEN_LABEL,
        );
        set_position_metrics(
            cfds.iter()
                .filter(|cfd| cfd.state == AggregatedState::Closed),
            STATUS_CLOSED_LABEL,
        );
        set_position_metrics(
            cfds.iter()
                .filter(|cfd| cfd.state == AggregatedState::Failed),
            STATUS_FAILED_LABEL,
        );
        set_position_metrics(
            cfds.iter()
                .filter(|cfd| cfd.state == AggregatedState::Rejected),
            STATUS_REJECTED_LABEL,
        );
        set_position_metrics(
            cfds.iter()
                .filter(|cfd| cfd.state == AggregatedState::Refunded),
            STATUS_REFUNDED_LABEL,
        );
    }

    fn set_position_metrics<'a>(cfds: impl Iterator<Item = &'a Cfd>, status: &str) {
        let (long, short): (Vec<_>, Vec<_>) = cfds.partition(|cfd| cfd.position == Position::Long);

        set_metrics_for(POSITION_LONG_LABEL, status, &long);
        set_metrics_for(POSITION_SHORT_LABEL, status, &short);
    }

    fn set_metrics_for(position_label: &str, status: &str, position: &[&Cfd]) {
        POSITION_QUANTITY_GAUGE
            .with(&HashMap::from([
                (POSITION_LABEL, position_label),
                (STATUS_LABEL, status),
            ]))
            .set(
                sum_amounts(position)
                    .into_decimal()
                    .to_f64()
                    .unwrap_or_default(),
            );
        POSITION_AMOUNT_GAUGE
            .with(&HashMap::from([
                (POSITION_LABEL, position_label),
                (STATUS_LABEL, status),
            ]))
            .set(position.len() as i64);
    }

    fn sum_amounts(cfds: &[&Cfd]) -> Usd {
        cfds.iter()
            .fold(Usd::ZERO, |sum, cfd| cfd.quantity_usd + sum)
    }
}
