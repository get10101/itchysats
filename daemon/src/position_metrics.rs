use crate::db;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use model::CfdEvent;
use model::EventKind;
use model::OrderId;
use model::Position;
use model::Usd;
use rust_decimal::Decimal;
use std::collections::HashMap;
use xtra_productivity::xtra_productivity;
use xtras::SendAsyncSafe;

pub struct Actor {
    db: db::Connection,
    state: State,
}

/// Internal struct to keep state in one place
struct State {
    cfds: Option<HashMap<OrderId, Cfd>>,
}

impl Actor {
    pub fn new(db: db::Connection) -> Self {
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

    async fn update_cfd(&mut self, db: db::Connection, id: OrderId) -> Result<()> {
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
            ContractSetupCompleted { .. } | LockConfirmed => Self {
                is_open: true,
                ..self
            },
            ContractSetupFailed => {
                // This is needed due to a bug that has since been fixed; `OfferRejected` and
                // `ContractSetupFailed` are mutually exclusive.
                // We give `OfferRejected` priority over `ContractSetupFailed`.
                if self.is_rejected {
                    Self { ..self }
                } else {
                    Self {
                        is_failed: true,
                        ..self
                    }
                }
            }
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
            LockConfirmedAfterFinality => Self {
                // This event is only appended if lock confirmation happens after we spent from lock
                // on chain. This is for special case where collaborative settlement is triggered
                // before lock is confirmed. In such a case the CFD is closed
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
            settlement,
            ..
        } = closed_cfd;

        let quantity_usd = Usd::new(Decimal::from(u64::from(n_contracts)));

        let (is_refunded, is_closed) = match settlement {
            db::Settlement::Collaborative { .. } | db::Settlement::Cet { .. } => (false, true),
            db::Settlement::Refund { .. } => (true, false),
        };

        Self {
            id,
            position,
            quantity_usd,

            is_open: false,
            is_closed,
            is_failed: false,
            is_refunded,
            is_rejected: false,
            version: 0,
        }
    }
}

impl db::FailedCfdAggregate for Cfd {
    fn new_failed(_: Self::CtorArgs, cfd: db::FailedCfd) -> Self {
        let db::FailedCfd {
            id,
            position,
            n_contracts,
            kind,
            ..
        } = cfd;

        let quantity_usd = Usd::new(Decimal::from(u64::from(n_contracts)));

        let (is_failed, is_rejected) = match kind {
            db::Kind::OfferRejected => (false, true),
            db::Kind::ContractSetupFailed => (true, false),
        };

        Self {
            id,
            position,
            quantity_usd,

            is_open: false,
            is_closed: false,
            is_failed,
            is_refunded: false,
            is_rejected,
            version: 0,
        }
    }
}

mod metrics {
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

    pub fn update_position_metrics(cfds: HashMap<OrderId, Cfd>) {
        let cfds = cfds.into_iter().map(|(_, cfd)| cfd).collect::<Vec<_>>();

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
                        is_failed = cfd.is_failed,
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
