use crate::db;
use async_trait::async_trait;
use futures::StreamExt;
use model::CfdEvent;
use model::EventKind;
use model::OrderId;
use model::Position;
use model::Usd;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

// TODO: ideally this would be more often
pub const UPDATE_METRIC_INTERVAL: Duration = Duration::from_secs(60);

pub struct Actor {
    tasks: Tasks,
    db: db::Connection,
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
            .add(this.send_interval(UPDATE_METRIC_INTERVAL, || UpdateMetrics));
    }

    async fn stopped(self) -> Self::Stop {}
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: UpdateMetrics) {
        tracing::debug!("Collecting metrics");
        let mut stream = self.db.load_all_cfds::<Cfd>(());

        let mut cfds = Vec::new();
        while let Some(cfd) = stream.next().await {
            let cfd = match cfd {
                Ok(cfd) => cfd,
                Err(e) => {
                    tracing::error!("Failed to rehydrate CFD: {e:#}");
                    continue;
                }
            };
            cfds.push(cfd);
        }

        metrics::update_position_metrics(cfds.as_slice());
    }
}

#[derive(Debug)]
struct UpdateMetrics;

/// Read-model of the CFD for the position metrics actor.
#[derive(Clone, Copy)]
pub struct Cfd {
    id: OrderId,
    position: Position,
    quantity_usd: Usd,

    is_open: bool,
    is_closed: bool,
    is_failed: bool,
    is_refunded: bool,
    is_rejected: bool,

    version: u32,
}

impl db::CfdAggregate for Cfd {
    type CtorArgs = ();

    fn new(_: Self::CtorArgs, cfd: db::Cfd) -> Self {
        Self {
            id: cfd.id,
            position: cfd.position,
            quantity_usd: cfd.quantity_usd,
            is_open: false,
            is_closed: false,
            is_failed: false,
            is_refunded: false,
            is_rejected: false,
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
                is_open: false,
                is_closed: false,
                is_failed: false,
                is_refunded: false,
                ..self
            },
            ContractSetupCompleted { .. } | LockConfirmed | LockConfirmedAfterFinality => Self {
                is_open: true,
                ..self
            },
            ContractSetupFailed => Self {
                is_failed: true,
                ..self
            },
            OfferRejected => Self {
                is_rejected: true,
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
                is_open: false,
                is_closed: true,
                ..self
            },
            ManualCommit { .. } | CommitConfirmed => Self {
                // we don't know yet if the position will be closed immediately (e.g. through
                // punishing) or a bit later after the oracle has attested to the price
                ..self
            },
            CetConfirmed => Self {
                is_open: false,
                is_closed: true,
                ..self
            },
            RefundConfirmed => Self {
                is_open: false,
                is_refunded: true,
                ..self
            },
            RevokeConfirmed => Self {
                // the other party was punished, we are done here!
                is_open: false,
                is_closed: true,
                ..self
            },
            CollaborativeSettlementConfirmed => Self {
                is_open: false,
                is_closed: true,
                ..self
            },
            CetTimelockExpiredPriorOracleAttestation
            | CetTimelockExpiredPostOracleAttestation { .. } => Self {
                is_open: false,
                is_closed: true,
                ..self
            },
            RefundTimelockExpired { .. } => Self {
                // a rollover with an expired timelock should be rejected for settlement and
                // rollover, hence, this is closed
                is_open: false,
                is_closed: true,
                ..self
            },
            OracleAttestedPriorCetTimelock { .. } | OracleAttestedPostCetTimelock { .. } => Self {
                // we know the closing price already and can assume that the cfd will be closed
                // accordingly
                is_open: false,
                is_closed: true,
                ..self
            },
        }
    }
}

impl db::ClosedCfdAggregate for Cfd {
    fn new_closed(_: Self::CtorArgs, closed_cfd: db::ClosedCfd) -> Self {
        let db::ClosedCfd {
            id,
            position,
            n_contracts,
            ..
        } = closed_cfd;

        let quantity_usd =
            Usd::new(Decimal::from_u64(u64::from(n_contracts)).expect("u64 to fit into Decimal"));

        Self {
            id,
            position,
            quantity_usd,

            is_open: false,
            is_closed: true,
            is_failed: false,
            is_refunded: false,
            is_rejected: false,
            version: 0,
        }
    }
}

mod metrics {
    use crate::position_metrics::Cfd;
    use model::Position;
    use model::Usd;
    use rust_decimal::prelude::ToPrimitive;
    use std::collections::HashMap;

    const POSITION_LABEL: &str = "position";
    const POSITION_LONG_LABEL: &str = "long";
    const POSITION_SHORT_LABEL: &str = "short";

    const STATUS_LABEL: &str = "status";
    const STATUS_OPEN_LABEL: &str = "open";
    const STATUS_CLOSED_LABEL: &str = "closed";
    const STATUS_FAILED_LABEL: &str = "failed";
    const STATUS_REJECTED_LABEL: &str = "rejected";
    const STATUS_REFUNDED_LABEL: &str = "refunded";
    // this is needed so that we do not lose cfds which are in a weird state, e.g. open & closed.
    // This should be 0 though but for the time being we add it
    const STATUS_UNKNOWN_LABEL: &str = "unknown";

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

    pub fn update_position_metrics(cfds: &[Cfd]) {
        set_position_metrics(cfds.iter().filter(|cfd| cfd.is_open), STATUS_OPEN_LABEL);
        set_position_metrics(cfds.iter().filter(|cfd| cfd.is_closed), STATUS_CLOSED_LABEL);
        set_position_metrics(cfds.iter().filter(|cfd| cfd.is_failed), STATUS_FAILED_LABEL);
        set_position_metrics(
            cfds.iter().filter(|cfd| cfd.is_rejected),
            STATUS_REJECTED_LABEL,
        );
        set_position_metrics(
            cfds.iter().filter(|cfd| cfd.is_refunded),
            STATUS_REFUNDED_LABEL,
        );

        set_position_metrics(
            cfds.iter().filter(|cfd| {
                let unknown_state = is_unknown(cfd);
                if unknown_state {
                    tracing::error!(
                        is_open = cfd.is_open,
                        is_closed = cfd.is_closed,
                        is_refunded = cfd.is_refunded,
                        is_rejected = cfd.is_rejected,
                        order_id = %cfd.id,
                        "CFD is in weird state"
                    );
                }
                unknown_state
            }),
            STATUS_UNKNOWN_LABEL,
        );
    }

    /// Return true if a CFD is in multiple states
    fn is_unknown(cfd: &Cfd) -> bool {
        !(cfd.is_open ^ cfd.is_closed ^ cfd.is_refunded ^ cfd.is_rejected ^ cfd.is_failed)
            && (cfd.is_open || cfd.is_closed || cfd.is_refunded || cfd.is_rejected || cfd.is_failed)
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

    #[cfg(test)]
    mod tests {
        use super::*;

        fn create_cfd(state_flags: StateFlags) -> Cfd {
            Cfd {
                id: Default::default(),
                position: Position::Long,
                quantity_usd: Usd::ZERO,
                is_open: state_flags.open,
                is_closed: state_flags.closed,
                is_failed: state_flags.failed,
                is_refunded: state_flags.refunded,
                is_rejected: state_flags.rejected,
                version: 0,
            }
        }

        #[derive(Debug, Clone, Copy)]
        struct StateFlags {
            open: bool,
            closed: bool,
            failed: bool,
            refunded: bool,
            rejected: bool,
        }

        #[test]
        fn when_cfd_is_in_single_no_state_then_is_not_unknown() {
            let state_matrix = [
                [false, false, false, false, false],
                [true, false, false, false, false],
                [false, true, false, false, false],
                [false, false, true, false, false],
                [false, false, false, true, false],
                [false, false, false, false, true],
            ];

            for states in state_matrix {
                let is_open = states[0];
                let is_closed = states[1];
                let is_failed = states[2];
                let is_refunded = states[3];
                let is_rejected = states[4];
                let state_flags = StateFlags {
                    open: is_open,
                    closed: is_closed,
                    failed: is_failed,
                    refunded: is_refunded,
                    rejected: is_rejected,
                };
                let cfd = create_cfd(state_flags);

                assert!(
                    !is_unknown(&cfd),
                    "State combination was unknown. {state_flags:?}"
                );
            }
        }

        #[test]
        fn when_cfd_is_in_multiple_states_then_is_unknown() {
            let state_matrix = [
                [true, true, false, false, false],
                [true, false, true, false, false],
                [true, false, false, true, false],
                [true, false, false, false, true],
                [false, true, true, false, false],
                [false, true, false, true, false],
                [false, true, false, false, true],
                [false, false, true, true, false],
                [false, false, true, false, true],
                [false, false, false, true, true],
            ];
            for states in state_matrix {
                let is_open = states[0];
                let is_closed = states[1];
                let is_failed = states[2];
                let is_refunded = states[3];
                let is_rejected = states[4];

                let state_flags = StateFlags {
                    open: is_open,
                    closed: is_closed,
                    failed: is_failed,
                    refunded: is_refunded,
                    rejected: is_rejected,
                };
                let cfd = create_cfd(state_flags);
                assert!(
                    is_unknown(&cfd),
                    "State combination was not unknown. {state_flags:?}"
                );
            }
        }
    }
}
