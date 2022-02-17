use crate::db::append_event;
use crate::monitor;
use crate::monitor::MonitorCetFinality;
use crate::monitor::MonitorParams;
use crate::monitor::TransactionKind;
use crate::oracle;
use crate::projection;
use anyhow::Result;
use async_trait::async_trait;
use model::CfdEvent;
use model::EventKind;
use model::Role;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;
use xtras::SendAsyncSafe;

pub struct Actor {
    db: sqlx::SqlitePool,
    role: Role,
    cfds_changed: Box<dyn MessageChannel<projection::CfdChanged>>,
    try_broadcast_transaction: Box<dyn MessageChannel<monitor::TryBroadcastTransaction>>,
    start_monitoring: Box<dyn MessageChannel<monitor::StartMonitoring>>,
    monitor_cet_finality: Box<dyn MessageChannel<monitor::MonitorCetFinality>>,
    monitor_collaborative_settlement: Box<dyn MessageChannel<monitor::CollaborativeSettlement>>,
    monitor_attestation: Box<dyn MessageChannel<oracle::MonitorAttestation>>,
}

pub struct Event(CfdEvent);

impl Event {
    pub fn new(event: CfdEvent) -> Self {
        Self(event)
    }
}

impl Actor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: sqlx::SqlitePool,
        role: Role,
        cfds_changed: &(impl MessageChannel<projection::CfdChanged> + 'static),
        try_broadcast_transaction: &(impl MessageChannel<monitor::TryBroadcastTransaction> + 'static),
        start_monitoring: &(impl MessageChannel<monitor::StartMonitoring> + 'static),
        monitor_cet: &(impl MessageChannel<monitor::MonitorCetFinality> + 'static),
        monitor_collaborative_settlement: &(impl MessageChannel<monitor::CollaborativeSettlement>
              + 'static),
        monitor_attestation: &(impl MessageChannel<oracle::MonitorAttestation> + 'static),
    ) -> Self {
        Self {
            db,
            role,
            cfds_changed: cfds_changed.clone_channel(),
            try_broadcast_transaction: try_broadcast_transaction.clone_channel(),
            start_monitoring: start_monitoring.clone_channel(),
            monitor_cet_finality: monitor_cet.clone_channel(),
            monitor_collaborative_settlement: monitor_collaborative_settlement.clone_channel(),
            monitor_attestation: monitor_attestation.clone_channel(),
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

        // 2. Post process event
        use EventKind::*;
        match event.event {
            ContractSetupCompleted { dlc, .. } => {
                tracing::info!("Setup complete, publishing on chain now");

                let lock_tx = dlc.lock.0.clone();
                self.try_broadcast_transaction
                    .send_async_safe(monitor::TryBroadcastTransaction {
                        tx: lock_tx,
                        kind: TransactionKind::Lock,
                    })
                    .await?;

                self.start_monitoring
                    .send_async_safe(monitor::StartMonitoring {
                        id: event.id,
                        params: MonitorParams::new(dlc.clone()),
                    })
                    .await?;

                self.monitor_attestation
                    .send_async_safe(oracle::MonitorAttestation {
                        event_id: dlc.settlement_event_id,
                    })
                    .await?;
            }
            CollaborativeSettlementCompleted {
                spend_tx, script, ..
            } => {
                let txid = spend_tx.txid();

                match self.role {
                    Role::Maker => {
                        self.try_broadcast_transaction
                            .send_async_safe(monitor::TryBroadcastTransaction {
                                tx: spend_tx,
                                kind: TransactionKind::CollaborativeClose,
                            })
                            .await?;
                    }
                    Role::Taker => {
                        // TODO: Publish the tx once the collaborative settlement is symmetric,
                        // allowing the taker to publish as well.

                        tracing::info!(order_id=%event.id, "Collaborative settlement completed successfully {txid}");
                    }
                };

                self.monitor_collaborative_settlement
                    .send_async_safe(monitor::CollaborativeSettlement {
                        order_id: event.id,
                        tx: (txid, script),
                    })
                    .await?;
            }
            CetTimelockExpiredPostOracleAttestation { cet } => {
                let _ = self
                    .monitor_cet_finality
                    .send_async_safe(MonitorCetFinality {
                        order_id: event.id,
                        cet: cet.clone(),
                    })
                    .await?;
                self.try_broadcast_transaction
                    .send_async_safe(monitor::TryBroadcastTransaction {
                        tx: cet,
                        kind: TransactionKind::Cet,
                    })
                    .await?;
            }
            OracleAttestedPostCetTimelock { cet, .. } => {
                let _ = self
                    .monitor_cet_finality
                    .send_async_safe(MonitorCetFinality {
                        order_id: event.id,
                        cet: cet.clone(),
                    })
                    .await?;
                self.try_broadcast_transaction
                    .send_async_safe(monitor::TryBroadcastTransaction {
                        tx: cet,
                        kind: TransactionKind::Cet,
                    })
                    .await?;
            }
            OracleAttestedPriorCetTimelock {
                commit_tx: Some(tx),
                timelocked_cet: cet,
                ..
            } => {
                let _ = self
                    .monitor_cet_finality
                    .send_async_safe(MonitorCetFinality {
                        order_id: event.id,
                        cet,
                    })
                    .await?;
                self.try_broadcast_transaction
                    .send_async_safe(monitor::TryBroadcastTransaction {
                        tx,
                        kind: TransactionKind::Cet,
                    })
                    .await?;
            }
            ManualCommit { tx } => {
                self.try_broadcast_transaction
                    .send_async_safe(monitor::TryBroadcastTransaction {
                        tx,
                        kind: TransactionKind::Commit,
                    })
                    .await?;
            }
            OracleAttestedPriorCetTimelock {
                commit_tx: None,
                timelocked_cet: cet,
                ..
            } => {
                let _ = self
                    .monitor_cet_finality
                    .send_async_safe(MonitorCetFinality {
                        order_id: event.id,
                        cet,
                    })
                    .await?;
            }
            RolloverCompleted { dlc, .. } => {
                tracing::info!(order_id=%event.id, "Rollover complete");

                self.start_monitoring
                    .send_async_safe(monitor::StartMonitoring {
                        id: event.id,
                        params: MonitorParams::new(dlc.clone()),
                    })
                    .await?;

                self.monitor_attestation
                    .send_async_safe(oracle::MonitorAttestation {
                        event_id: dlc.settlement_event_id,
                    })
                    .await?;
            }
            RefundTimelockExpired { refund_tx: tx } => {
                self.try_broadcast_transaction
                    .send_async_safe(monitor::TryBroadcastTransaction {
                        tx,
                        kind: TransactionKind::Refund,
                    })
                    .await?;
            }
            RefundConfirmed => {
                tracing::info!(order_id=%event.id, "Refund transaction confirmed");
            }
            CollaborativeSettlementStarted { .. }
            | ContractSetupStarted
            | ContractSetupFailed
            | OfferRejected
            | RolloverStarted
            | RolloverAccepted
            | RolloverRejected
            | RolloverFailed
            | CollaborativeSettlementProposalAccepted
            | LockConfirmed
            | LockConfirmedAfterFinality
            | CommitConfirmed
            | CetConfirmed
            | RevokeConfirmed
            | CollaborativeSettlementConfirmed
            | CollaborativeSettlementRejected
            | CollaborativeSettlementFailed
            | CetTimelockExpiredPriorOracleAttestation => {}
        }

        // 3. Update UI
        self.cfds_changed
            .send_async_safe(projection::CfdChanged(event.id))
            .await?;

        Ok(())
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
