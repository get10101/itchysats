use crate::monitor::MonitorAfterContractSetup;
use crate::monitor::MonitorAfterRollover;
use crate::monitor::MonitorCetFinality;
use crate::monitor::MonitorCollaborativeSettlement;
use crate::monitor::TransactionKind;
use crate::monitor::TryBroadcastTransaction;
use crate::oracle;
use crate::position_metrics;
use crate::projection;
use anyhow::Result;
use async_trait::async_trait;
use model::CfdEvent;
use model::EventKind;
use model::Role;
use sqlite_db;
use tracing::Instrument;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;
use xtras::SendAsyncSafe;

pub struct Actor {
    db: sqlite_db::Connection,
    role: Role,
    cfds_changed: MessageChannel<projection::CfdChanged, ()>,
    cfd_changed_metrics: MessageChannel<position_metrics::CfdChanged, ()>,
    try_broadcast_transaction: MessageChannel<TryBroadcastTransaction, Result<()>>,
    monitor_after_contract_setup: MessageChannel<MonitorAfterContractSetup, ()>,
    monitor_after_rollover: MessageChannel<MonitorAfterRollover, ()>,
    monitor_cet_finality: MessageChannel<MonitorCetFinality, Result<()>>,
    monitor_collaborative_settlement: MessageChannel<MonitorCollaborativeSettlement, ()>,
    monitor_attestation: MessageChannel<oracle::MonitorAttestations, ()>,
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
        db: sqlite_db::Connection,
        role: Role,
        cfds_changed: MessageChannel<projection::CfdChanged, ()>,
        cfd_changed_metrics: MessageChannel<position_metrics::CfdChanged, ()>,
        try_broadcast_transaction: MessageChannel<TryBroadcastTransaction, Result<()>>,
        monitor_after_contract_setup: MessageChannel<MonitorAfterContractSetup, ()>,
        monitor_after_rollover: MessageChannel<MonitorAfterRollover, ()>,
        monitor_cet_finality: MessageChannel<MonitorCetFinality, Result<()>>,
        monitor_collaborative_settlement: MessageChannel<MonitorCollaborativeSettlement, ()>,
        monitor_attestation: MessageChannel<oracle::MonitorAttestations, ()>,
    ) -> Self {
        Self {
            db,
            role,
            cfds_changed,
            cfd_changed_metrics,
            try_broadcast_transaction,
            monitor_after_contract_setup,
            monitor_after_rollover,
            monitor_cet_finality,
            monitor_collaborative_settlement,
            monitor_attestation,
        }
    }
}

#[xtra_productivity]
impl Actor {
    fn handle(&mut self, msg: Event) -> Result<()> {
        let event = msg.0;

        // 1. Safe in DB
        self.db.append_event(event.clone()).await?;

        // 2. Post process event
        use EventKind::*;
        match event.event {
            ContractSetupCompleted { dlc: Some(dlc), .. } => {
                let lock_tx = dlc.lock.0.clone();

                let span = tracing::debug_span!("Broadcast lock TX", order_id = %event.id);
                self.try_broadcast_transaction
                    .send_async_safe(TryBroadcastTransaction {
                        tx: lock_tx,
                        kind: TransactionKind::Lock,
                    })
                    .instrument(span)
                    .await?;

                self.monitor_after_contract_setup
                    .send_async_safe(MonitorAfterContractSetup::new(event.id, &dlc))
                    .await?;

                self.monitor_attestation
                    .send_async_safe(oracle::MonitorAttestations {
                        event_ids: dlc.event_ids(),
                    })
                    .await?;
            }
            CollaborativeSettlementCompleted {
                spend_tx, script, ..
            } => {
                let txid = spend_tx.txid();

                match self.role {
                    Role::Maker => {
                        let span = tracing::debug_span!(
                            "Broadcast collaborative settlement TX",
                            order_id = %event.id
                        );
                        self.try_broadcast_transaction
                            .send_async_safe(TryBroadcastTransaction {
                                tx: spend_tx,
                                kind: TransactionKind::CollaborativeClose,
                            })
                            .instrument(span)
                            .await?;
                    }
                    Role::Taker => {
                        // TODO: Publish the tx once the collaborative settlement is symmetric,
                        // allowing the taker to publish as well.
                    }
                };

                self.monitor_collaborative_settlement
                    .send_async_safe(MonitorCollaborativeSettlement {
                        order_id: event.id,
                        tx: (txid, script),
                    })
                    .await?;
            }
            CetTimelockExpiredPostOracleAttestation { cet }
            | OracleAttestedPostCetTimelock { cet, .. } => {
                let _ = self
                    .monitor_cet_finality
                    .send_async_safe(MonitorCetFinality {
                        order_id: event.id,
                        cet: cet.clone(),
                    })
                    .await?;
                let span = tracing::debug_span!("Broadcast CET", order_id = %event.id);
                self.try_broadcast_transaction
                    .send_async_safe(TryBroadcastTransaction {
                        tx: cet,
                        kind: TransactionKind::Cet,
                    })
                    .instrument(span)
                    .await?;
            }
            OracleAttestedPriorCetTimelock {
                commit_tx: Some(tx),
                ..
            }
            | ManualCommit { tx } => {
                let span = tracing::debug_span!("Broadcast commit TX", order_id = %event.id);
                self.try_broadcast_transaction
                    .send_async_safe(TryBroadcastTransaction {
                        tx,
                        kind: TransactionKind::Commit,
                    })
                    .instrument(span)
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
            RolloverCompleted { dlc: Some(dlc), .. } => {
                self.monitor_after_rollover
                    .send_async_safe(MonitorAfterRollover::new(event.id, &dlc))
                    .await?;

                self.monitor_attestation
                    .send_async_safe(oracle::MonitorAttestations {
                        event_ids: dlc.event_ids(),
                    })
                    .await?;
            }
            RefundTimelockExpired { refund_tx: tx } => {
                let span = tracing::debug_span!("Broadcast refund TX", order_id = %event.id);
                self.try_broadcast_transaction
                    .send_async_safe(TryBroadcastTransaction {
                        tx,
                        kind: TransactionKind::Refund,
                    })
                    .instrument(span)
                    .await?;
            }
            ContractSetupCompleted { dlc: None, .. }
            | RolloverCompleted { dlc: None, .. }
            | RefundConfirmed
            | CollaborativeSettlementStarted { .. }
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

        // 4. Update metrics
        self.cfd_changed_metrics
            .send_async_safe(position_metrics::CfdChanged(event.id))
            .await?;

        Ok(())
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
