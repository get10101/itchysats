use crate::db::append_event;
use crate::model::cfd;
use crate::model::cfd::CfdEvent;
use crate::model::cfd::Role;
use crate::monitor;
use crate::monitor::MonitorParams;
use crate::oracle;
use crate::projection;
use anyhow::Context;
use anyhow::Result;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    db: sqlx::SqlitePool,
    role: Role,
    cfds_changed: Box<dyn MessageChannel<projection::CfdsChanged>>,
    try_broadcast_transaction: Box<dyn MessageChannel<monitor::TryBroadcastTransaction>>,
    start_monitoring: Box<dyn MessageChannel<monitor::StartMonitoring>>,
    monitor_collaborative_settlement: Box<dyn MessageChannel<monitor::CollaborativeSettlement>>,
    monitor_attestation: Box<dyn MessageChannel<oracle::MonitorAttestation>>,
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
        try_broadcast_transaction: &(impl MessageChannel<monitor::TryBroadcastTransaction> + 'static),
        start_monitoring: &(impl MessageChannel<monitor::StartMonitoring> + 'static),
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
        use CfdEvent::*;
        match event.event {
            ContractSetupCompleted { dlc } => {
                tracing::info!("Setup complete, publishing on chain now");

                let lock_tx = dlc.lock.0.clone();
                let txid = self
                    .try_broadcast_transaction
                    .send(monitor::TryBroadcastTransaction { tx: lock_tx })
                    .await??;

                tracing::info!("Lock transaction published with txid {}", txid);

                self.start_monitoring
                    .send(monitor::StartMonitoring {
                        id: event.id,
                        params: MonitorParams::new(dlc.clone()),
                    })
                    .await?;

                self.monitor_attestation
                    .send(oracle::MonitorAttestation {
                        event_id: dlc.settlement_event_id,
                    })
                    .await?;
            }
            CollaborativeSettlementCompleted {
                spend_tx, script, ..
            } => {
                let txid = match self.role {
                    Role::Maker => {
                        let txid = self
                            .try_broadcast_transaction
                            .send(monitor::TryBroadcastTransaction { tx: spend_tx })
                            .await?
                            .context("Broadcasting close transaction")?;

                        tracing::info!(order_id=%event.id, "Close transaction published with txid {}", txid);

                        txid
                    }
                    Role::Taker => {
                        // TODO: Publish the tx once the collaborative settlement is symmetric,
                        // allowing the taker to publish as well.
                        let txid = spend_tx.txid();
                        tracing::info!(order_id=%event.id, "Collaborative settlement completed successfully {}", txid);
                        txid
                    }
                };

                self.monitor_collaborative_settlement
                    .send(monitor::CollaborativeSettlement {
                        order_id: event.id,
                        tx: (txid, script),
                    })
                    .await?;
            }
            CollaborativeSettlementRejected { commit_tx } => {
                let txid = self
                    .try_broadcast_transaction
                    .send(monitor::TryBroadcastTransaction { tx: commit_tx })
                    .await?
                    .context("Broadcasting commit transaction")?;

                tracing::info!(
                    "Closing non-collaboratively. Commit tx published with txid {}",
                    txid
                )
            }
            CollaborativeSettlementFailed { commit_tx } => {
                let txid = self
                    .try_broadcast_transaction
                    .send(monitor::TryBroadcastTransaction { tx: commit_tx })
                    .await?
                    .context("Broadcasting commit transaction")?;

                tracing::warn!(
                    "Closing non-collaboratively. Commit tx published with txid {}",
                    txid
                )
            }
            OracleAttestedPostCetTimelock { cet, .. }
            | CetTimelockConfirmedPostOracleAttestation { cet } => {
                let txid = self
                    .try_broadcast_transaction
                    .send(monitor::TryBroadcastTransaction { tx: cet })
                    .await?
                    .context("Failed to broadcast CET")?;

                tracing::info!(%txid, "CET published");
            }
            OracleAttestedPriorCetTimelock { commit_tx: tx, .. } | ManualCommit { tx } => {
                let txid = self
                    .try_broadcast_transaction
                    .send(monitor::TryBroadcastTransaction { tx })
                    .await?
                    .context("Failed to broadcast commit transaction")?;

                tracing::info!(%txid, "Commit transaction published");
            }
            RolloverCompleted { dlc } => {
                tracing::info!(order_id=%event.id, "Rollover complete");

                self.start_monitoring
                    .send(monitor::StartMonitoring {
                        id: event.id,
                        params: MonitorParams::new(dlc.clone()),
                    })
                    .await?;

                self.monitor_attestation
                    .send(oracle::MonitorAttestation {
                        event_id: dlc.settlement_event_id,
                    })
                    .await?;
            }
            RefundTimelockExpired { refund_tx: tx } => {
                let txid = self
                    .try_broadcast_transaction
                    .send(monitor::TryBroadcastTransaction { tx })
                    .await?
                    .context("Failed to broadcast refund transaction")?;

                tracing::info!(order_id=%event.id, "Refund transaction published: {}", txid);
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
            | CommitConfirmed
            | CetConfirmed
            | RevokeConfirmed
            | CollaborativeSettlementConfirmed
            | CetTimelockConfirmedPriorOracleAttestation => {}
        }

        // 3. Update UI
        self.cfds_changed.send(projection::CfdsChanged).await?;

        Ok(())
    }
}

impl xtra::Actor for Actor {}
