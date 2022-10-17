use crate::bitcoin::consensus::encode::serialize_hex;
use crate::bitcoin::Transaction;
use crate::command;
use crate::wallet::RpcErrorCode;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::PublicKey;
use bdk::bitcoin::Script;
use bdk::bitcoin::Txid;
use bdk::descriptor::Descriptor;
use bdk::electrum_client;
use bdk::electrum_client::ElectrumApi;
use bdk::electrum_client::GetHistoryRes;
use bdk::miniscript::DescriptorTrait;
use btsieve::BlockHeight;
use btsieve::ScriptStatus;
use btsieve::State;
use btsieve::TxStatus;
use futures::StreamExt;
use model::CfdEvent;
use model::Dlc;
use model::EventKind;
use model::OrderId;
use model::CET_TIMELOCK;
use serde_json::Value;
use sqlite_db;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::Instrument;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

const LOCK_FINALITY_CONFIRMATIONS: u32 = 1;
const CLOSE_FINALITY_CONFIRMATIONS: u32 = 3;
const COMMIT_FINALITY_CONFIRMATIONS: u32 = 1;
const CET_FINALITY_CONFIRMATIONS: u32 = 3;
const REFUND_FINALITY_CONFIRMATIONS: u32 = 3;
const BATCH_SIZE: usize = 25;

pub struct MonitorAfterContractSetup {
    order_id: OrderId,
    transactions: TransactionsAfterContractSetup,
}

pub struct MonitorAfterRollover {
    order_id: OrderId,
    transactions: TransactionsAfterRollover,
}

pub struct MonitorCollaborativeSettlement {
    pub order_id: OrderId,
    pub tx: (Txid, Script),
}

pub struct MonitorCetFinality {
    pub order_id: OrderId,
    pub cet: Transaction,
}

pub struct TryBroadcastTransaction {
    pub tx: Transaction,
    pub kind: TransactionKind,
}

#[derive(Clone, Copy)]
pub enum TransactionKind {
    Lock,
    Commit,
    Refund,
    CollaborativeClose,
    Cet,
}

impl TransactionKind {
    fn name(&self) -> &'static str {
        match self {
            TransactionKind::Lock => "lock",
            TransactionKind::Commit => "commit",
            TransactionKind::Refund => "refund",
            TransactionKind::CollaborativeClose => "collaborative-close",
            TransactionKind::Cet => "contract-execution",
        }
    }
}

fn parse_rpc_protocol_error(error_value: &Value) -> Result<RpcError> {
    let json = error_value
        .as_str()
        .context("Not a string")?
        .split_terminator("RPC error: ")
        .nth(1)
        .context("Unknown error code format")?;

    let error = serde_json::from_str::<RpcError>(json).context("Error has unexpected format")?;

    Ok(error)
}

#[derive(serde::Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Clone, Copy)]
pub struct Sync;

// TODO: Send messages to the projection actor upon finality events so we send out updates.
//  -> Might as well just send out all events independent of sending to the cfd actor.
pub struct Actor {
    executor: command::Executor,
    client: Arc<bdk::electrum_client::Client>,
    state: State<Event>,
    db: sqlite_db::Connection,
}

/// Read-model of the CFD for the monitoring actor.
#[derive(Clone)]
struct Cfd {
    id: OrderId,

    lock: Option<Lock>,
    monitor_lock_finality: bool,

    collaborative_settlement: Option<(Txid, Script)>,
    monitor_collaborative_settlement_finality: bool,

    commit: Option<Commit>,
    monitor_commit_finality: bool,
    monitor_cet_timelock: bool,
    monitor_refund_timelock: bool,

    cet: Option<(Txid, Script)>,
    monitor_cet_finality: bool,

    refund: Option<Refund>,
    monitor_refund_finality: bool,

    monitor_revoked_commit_transactions: Vec<RevokedCommit>,

    // Rebroadcast transactions upon startup
    broadcast_lock: Option<Transaction>,
    broadcast_cet: Option<Transaction>,
    broadcast_commit: Option<Transaction>,

    version: u32,
}

impl sqlite_db::CfdAggregate for Cfd {
    type CtorArgs = ();

    fn new(_: Self::CtorArgs, cfd: sqlite_db::Cfd) -> Self {
        Self {
            id: cfd.id,
            lock: None,
            monitor_lock_finality: false,
            collaborative_settlement: None,
            monitor_collaborative_settlement_finality: false,
            commit: None,
            monitor_commit_finality: false,
            monitor_cet_timelock: false,
            monitor_refund_timelock: false,
            cet: None,
            monitor_cet_finality: false,
            refund: None,
            monitor_refund_finality: false,
            monitor_revoked_commit_transactions: Vec::new(),
            broadcast_lock: None,
            broadcast_cet: None,
            broadcast_commit: None,
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
    // TODO: Ideally, we would only set the specific monitoring events to `true` that occur _next_,
    // like lock_finality after contract-setup. However, this would require that
    // - either the monitoring actor is smart enough to know that it needs to monitor for
    //   commit-finality after lock-finality
    // - or some other actor tells it to do that
    //
    // At the moment, neither of those two is the case which is why we set everything to true that
    // might become relevant. See also https://github.com/itchysats/itchysats/issues/605 and https://github.com/itchysats/itchysats/issues/236.
    fn apply(mut self, event: CfdEvent) -> Self {
        self.version += 1;

        use EventKind::*;
        match event.event {
            ContractSetupCompleted { dlc: Some(dlc), .. } => {
                let TransactionsAfterContractSetup {
                    lock,
                    commit,
                    refund,
                } = TransactionsAfterContractSetup::new(&dlc);

                Self {
                    lock: Some(lock),
                    monitor_lock_finality: true,
                    commit: Some(commit),
                    monitor_commit_finality: true,
                    monitor_cet_timelock: true,
                    monitor_refund_timelock: true,
                    refund: Some(refund),
                    monitor_refund_finality: true,
                    monitor_revoked_commit_transactions: Vec::new(),
                    broadcast_lock: Some(dlc.lock.0),
                    ..self
                }
            }
            RolloverCompleted { dlc: Some(dlc), .. } => {
                let TransactionsAfterRollover {
                    commit,
                    refund,
                    revoked_commits,
                } = TransactionsAfterRollover::new(&dlc);

                Self {
                    monitor_lock_finality: false,
                    commit: Some(commit),
                    monitor_commit_finality: true,
                    monitor_cet_timelock: true,
                    monitor_refund_timelock: true,
                    refund: Some(refund),
                    monitor_refund_finality: true,
                    monitor_revoked_commit_transactions: revoked_commits,
                    broadcast_lock: None,
                    ..self
                }
            }
            CollaborativeSettlementCompleted {
                spend_tx, script, ..
            } => {
                Self {
                    collaborative_settlement: Some((spend_tx.txid(), script)),
                    monitor_collaborative_settlement_finality: true,
                    monitor_lock_finality: false, // Lock is already final if we collab settle.
                    broadcast_lock: None,
                    ..self
                }
            }
            LockConfirmed | LockConfirmedAfterFinality => Self {
                monitor_lock_finality: false,
                broadcast_lock: None,
                ..self
            },
            ManualCommit { tx } => Self {
                broadcast_commit: Some(tx),
                ..self
            },
            CommitConfirmed => Self {
                monitor_commit_finality: false,
                broadcast_commit: None,
                ..self
            },
            // final states, don't monitor or re-broadcast anything
            CetConfirmed | RefundConfirmed | CollaborativeSettlementConfirmed => Self {
                monitor_lock_finality: false,
                monitor_commit_finality: false,
                monitor_cet_timelock: false,
                monitor_refund_timelock: false,
                monitor_refund_finality: false,
                monitor_revoked_commit_transactions: Vec::new(),
                monitor_collaborative_settlement_finality: false,
                monitor_cet_finality: false,
                broadcast_lock: None,
                broadcast_cet: None,
                broadcast_commit: None,
                ..self
            },
            CetTimelockExpiredPriorOracleAttestation => Self {
                monitor_cet_timelock: false,
                ..self
            },
            CetTimelockExpiredPostOracleAttestation { cet, .. }
            | OracleAttestedPostCetTimelock { cet, .. } => Self {
                broadcast_cet: Some(cet.clone()),
                cet: cet_txid_and_script(cet),
                monitor_cet_finality: true,
                monitor_cet_timelock: false,
                ..self
            },
            RefundTimelockExpired { .. } => Self {
                monitor_refund_timelock: false,
                ..self
            },
            ContractSetupCompleted { dlc: None, .. }
            | RolloverCompleted { dlc: None, .. }
            | RolloverStarted { .. }
            | RolloverAccepted
            | RolloverFailed
            | OracleAttestedPriorCetTimelock { .. }
            | CollaborativeSettlementStarted { .. }
            | CollaborativeSettlementRejected
            | CollaborativeSettlementFailed
            | CollaborativeSettlementProposalAccepted
            | ContractSetupStarted
            | ContractSetupFailed
            | OfferRejected
            | RolloverRejected => self,
            RevokeConfirmed => {
                // TODO: Implement revoked logic
                self
            }
        }
    }
}

fn cet_txid_and_script(cet: Transaction) -> Option<(Txid, Script)> {
    match cet.output.first() {
        Some(output) => Some((cet.txid(), output.script_pubkey.clone())),
        None => {
            tracing::error!("Failed to monitor cet using script pubkey because no TxOut's in CET");
            None
        }
    }
}

impl Actor {
    pub fn new(
        db: sqlite_db::Connection,
        electrum_rpc_url: String,
        executor: command::Executor,
    ) -> Result<Self> {
        let client = bdk::electrum_client::Client::new(&electrum_rpc_url)
            .context("Failed to initialize Electrum RPC client")?;

        // Initially fetch the latest block for storing the height.
        // We do not act on this subscription after this call.
        let latest_block = client
            .block_headers_subscribe()
            .context("Failed to subscribe to header notifications")?
            .height
            .into();

        Ok(Self {
            client: Arc::new(client),
            executor,
            state: State::new(latest_block),
            db,
        })
    }
}

impl Actor {
    fn monitor_lock_finality(&mut self, order_id: OrderId, Lock { txid, descriptor }: Lock) {
        self.state.monitor(
            txid,
            descriptor.script_pubkey(),
            ScriptStatus::with_confirmations(LOCK_FINALITY_CONFIRMATIONS),
            Event::LockFinality(order_id),
        )
    }

    fn monitor_commit_finality(&mut self, order_id: OrderId, Commit { txid, descriptor }: Commit) {
        self.state.monitor(
            txid,
            descriptor.script_pubkey(),
            ScriptStatus::with_confirmations(COMMIT_FINALITY_CONFIRMATIONS),
            Event::CommitFinality(order_id),
        )
    }

    fn monitor_close_finality(&mut self, order_id: OrderId, close_params: (Txid, Script)) {
        self.state.monitor(
            close_params.0,
            close_params.1,
            ScriptStatus::with_confirmations(CLOSE_FINALITY_CONFIRMATIONS),
            Event::CloseFinality(order_id),
        );
    }

    fn monitor_cet_finality(&mut self, order_id: OrderId, close_params: (Txid, Script)) {
        self.state.monitor(
            close_params.0,
            close_params.1,
            ScriptStatus::with_confirmations(CET_FINALITY_CONFIRMATIONS),
            Event::CetFinality(order_id),
        );
    }

    fn monitor_commit_cet_timelock(
        &mut self,
        order_id: OrderId,
        Commit { txid, descriptor }: Commit,
    ) {
        self.state.monitor(
            txid,
            descriptor.script_pubkey(),
            ScriptStatus::with_confirmations(CET_TIMELOCK),
            Event::CetTimelockExpired(order_id),
        );
    }

    fn monitor_commit_refund_timelock(
        &mut self,
        order_id: OrderId,
        Commit { txid, descriptor }: Commit,
        refund_timelock: u32,
    ) {
        self.state.monitor(
            txid,
            descriptor.script_pubkey(),
            ScriptStatus::with_confirmations(refund_timelock),
            Event::RefundTimelockExpired(order_id),
        );
    }

    fn monitor_refund_finality(
        &mut self,
        order_id: OrderId,
        Refund {
            txid,
            script_pubkey,
            ..
        }: Refund,
    ) {
        self.state.monitor(
            txid,
            script_pubkey,
            ScriptStatus::with_confirmations(REFUND_FINALITY_CONFIRMATIONS),
            Event::RefundFinality(order_id),
        );
    }

    fn monitor_revoked_commit_transactions(
        &mut self,
        order_id: OrderId,
        revoked_commits: Vec<RevokedCommit>,
    ) {
        for RevokedCommit {
            txid,
            script_pubkey,
        } in revoked_commits.into_iter()
        {
            self.state.monitor(
                txid,
                script_pubkey,
                ScriptStatus::InMempool,
                Event::RevokedTransactionFound(order_id),
            )
        }
    }

    #[tracing::instrument("Sync monitor", skip_all, err)]
    async fn sync(&mut self) -> Result<()> {
        // Fetch the latest block for storing the height.
        // We do not act on this subscription after this call, as we cannot rely on
        // subscription push notifications because eventually the Electrum server will
        // close the connection and subscriptions are not automatically renewed
        // upon renewing the connection.
        let latest_block_height = self
            .client
            .block_headers_subscribe()
            .context("Failed to subscribe to header notifications")?
            .height
            .into();

        let num_transactions = self.state.num_monitoring();

        tracing::trace!("Updating status of {num_transactions} transactions",);

        // TODO: Evaluate buffer; if we do batches of 25 and allow buffer of 100 here, does that
        // mean we can at most process 4 batches at the time?  What happens if the buffer is
        // exhausted? (I would expect the next messages will be queued) We use tokio::sync::
        // mpsc::channel because we need the receiver to be in an async context because we do async
        // work upon receiving script updates
        let (sender, mut receiver) = tokio::sync::mpsc::channel(100);

        let scripts = self
            .state
            .monitoring_scripts()
            .cloned()
            .collect::<Vec<Script>>();

        let batches = scripts.chunks(BATCH_SIZE).map(|batch| batch.to_owned());

        let client = self.client.clone();

        let batch_futures = batches.into_iter().map(move |batch| {
            tokio::task::spawn_blocking({
                let client = client.clone();
                let sender = sender.clone();
                move || {
                    for script in batch {
                        // TODO: Evaluate if we should notify the receivers about errors, I feel
                        //  logging is good enough here because the receiver won't know how to deal
                        //  with an error.
                        match client.script_get_history(&script) {
                            Ok(resp) => {
                                // We use blocking_send to stay within a sync context here
                                // One should not use async code in a spawn_blocking block
                                if let Err(e) = sender.blocking_send(resp) {
                                    tracing::error!("Error during script history batching: {e:#}")
                                }
                            }
                            Err(e) => {
                                tracing::error!("Error when fetching script history: {e:#}")
                            }
                        }
                    }
                }
            })
        });

        let joined_batch_futures = futures::future::join_all(batch_futures);
        tokio::spawn(joined_batch_futures);

        while let Some(script_history) = receiver.recv().await {
            // TODO: It's a bit weird that we pass in the latest block here several times but it
            // won't cause trouble 🤷‍  - We could refactor this logic at some point
            self.process_script_status_update(script_history, latest_block_height)
                .await;
        }

        Ok(())
    }

    async fn process_script_status_update(
        &mut self,
        script_history: Vec<GetHistoryRes>,
        latest_block_height: BlockHeight,
    ) {
        let status_list = script_history
            .into_iter()
            .map(|history| TxStatus {
                height: history.height,
                tx_hash: history.tx_hash,
            })
            .collect();

        let mut ready_events = self.state.update(latest_block_height, status_list);

        while let Some(event) = ready_events.pop() {
            match event {
                Event::LockFinality(id) => {
                    self.invoke_cfd_command(id, |cfd| Ok(Some(cfd.handle_lock_confirmed())))
                        .await
                }
                Event::CommitFinality(id) => {
                    self.invoke_cfd_command(id, |cfd| Ok(Some(cfd.handle_commit_confirmed())))
                        .await
                }
                Event::CloseFinality(id) => {
                    self.invoke_cfd_command(id, |cfd| {
                        Ok(Some(cfd.handle_collaborative_settlement_confirmed()))
                    })
                    .await
                }
                Event::CetTimelockExpired(id) => {
                    self.invoke_cfd_command(id, |cfd| cfd.handle_cet_timelock_expired().map(Some))
                        .await
                }
                Event::CetFinality(id) => {
                    self.invoke_cfd_command(id, |cfd| Ok(Some(cfd.handle_cet_confirmed())))
                        .await
                }
                Event::RefundFinality(id) => {
                    self.invoke_cfd_command(id, |cfd| Ok(Some(cfd.handle_refund_confirmed())))
                        .await
                }
                Event::RevokedTransactionFound(id) => {
                    self.invoke_cfd_command(id, |cfd| Ok(Some(cfd.handle_revoke_confirmed())))
                        .await
                }
                Event::RefundTimelockExpired(id) => {
                    self.invoke_cfd_command(id, |cfd| cfd.handle_refund_timelock_expired())
                        .await
                }
            }
        }
    }

    async fn invoke_cfd_command(
        &self,
        order_id: OrderId,
        handler: impl FnOnce(model::Cfd) -> Result<Option<CfdEvent>>,
    ) {
        match self.executor.execute(order_id, handler).await {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(%order_id, "Failed to update state of CFD: {e:#}");
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Copy)]
enum Event {
    LockFinality(OrderId),
    CommitFinality(OrderId),
    CloseFinality(OrderId),
    CetTimelockExpired(OrderId),
    CetFinality(OrderId),
    RefundTimelockExpired(OrderId),
    RefundFinality(OrderId),
    RevokedTransactionFound(OrderId),
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");
        tokio_extras::spawn(
            &this,
            this.clone().send_interval(
                Duration::from_secs(20),
                || Sync,
                xtras::IncludeSpan::Always,
            ),
        );

        tokio_extras::spawn_fallible(
            &this.clone(),
            {
                let db = self.db.clone();

                async move {
                    let mut stream = db.load_all_open_cfds::<Cfd>(());

                    while let Some(cfd) = stream.next().await {
                        let Cfd {
                            id,
                            lock,
                            monitor_lock_finality,
                            collaborative_settlement,
                            monitor_collaborative_settlement_finality,
                            commit,
                            monitor_commit_finality,
                            monitor_cet_timelock,
                            monitor_refund_timelock,
                            cet,
                            monitor_cet_finality,
                            refund,
                            monitor_refund_finality,
                            monitor_revoked_commit_transactions,
                            broadcast_lock,
                            broadcast_cet,
                            broadcast_commit,
                            ..
                        } = match cfd {
                            Ok(cfd) => cfd,
                            Err(e) => {
                                tracing::warn!("Failed to load CFD from database: {e:#}");
                                continue;
                            }
                        };
                        if let Some(tx) = broadcast_commit {
                            let span = tracing::debug_span!("Broadcast commit TX", order_id = %id);
                            if let Err(e) = this
                                .send(TryBroadcastTransaction {
                                    tx,
                                    kind: TransactionKind::Commit,
                                })
                                .instrument(span)
                                .await?
                            {
                                tracing::warn!("{e:#}")
                            }
                        }

                        if let Some(tx) = broadcast_cet {
                            let span = tracing::debug_span!("Broadcast CET", order_id = %id);
                            if let Err(e) = this
                                .send(TryBroadcastTransaction {
                                    tx,
                                    kind: TransactionKind::Cet,
                                })
                                .instrument(span)
                                .await?
                            {
                                tracing::warn!("{e:#}")
                            }
                        }

                        if let Some(tx) = broadcast_lock {
                            let span = tracing::debug_span!("Broadcast lock TX", order_id = %id);
                            if let Err(e) = this
                                .send(TryBroadcastTransaction {
                                    tx,
                                    kind: TransactionKind::Lock,
                                })
                                .instrument(span)
                                .await?
                            {
                                tracing::warn!("{e:#}")
                            }
                        }

                        this.send(ReinitMonitoring {
                            id,
                            lock,
                            monitor_lock_finality,
                            collaborative_settlement,
                            monitor_collaborative_settlement_finality,
                            commit,
                            monitor_commit_finality,
                            monitor_cet_timelock,
                            monitor_refund_timelock,
                            cet,
                            monitor_cet_finality,
                            refund,
                            monitor_refund_finality,
                            monitor_revoked_commit_transactions,
                        })
                        .await?;
                    }

                    anyhow::Ok(())
                }
            },
            |e| async move {
                tracing::warn!("Failed to re-initialize monitoring: {e:#}");
            },
        );
    }

    async fn stopped(self) -> Self::Stop {}
}

#[xtra_productivity]
impl Actor {
    async fn handle_monitor_after_contract_setup(&mut self, msg: MonitorAfterContractSetup) {
        let MonitorAfterContractSetup {
            order_id,
            transactions:
                TransactionsAfterContractSetup {
                    lock,
                    commit,
                    refund,
                },
        } = msg;

        self.monitor_lock_finality(order_id, lock);
        self.monitor_commit_finality(order_id, commit.clone());
        self.monitor_commit_cet_timelock(order_id, commit.clone());
        self.monitor_commit_refund_timelock(order_id, commit, refund.timelock);
        self.monitor_refund_finality(order_id, refund);
    }

    async fn handle_monitor_after_rollover(&mut self, msg: MonitorAfterRollover) {
        let MonitorAfterRollover {
            order_id,
            transactions:
                TransactionsAfterRollover {
                    commit,
                    refund,
                    revoked_commits,
                },
        } = msg;

        self.monitor_commit_finality(order_id, commit.clone());
        self.monitor_commit_cet_timelock(order_id, commit.clone());
        self.monitor_commit_refund_timelock(order_id, commit, refund.timelock);
        self.monitor_refund_finality(order_id, refund);
        self.monitor_revoked_commit_transactions(order_id, revoked_commits)
    }

    fn handle_collaborative_settlement(
        &mut self,
        collaborative_settlement: MonitorCollaborativeSettlement,
    ) {
        self.monitor_close_finality(
            collaborative_settlement.order_id,
            collaborative_settlement.tx,
        );
    }

    async fn handle_try_broadcast_transaction(&self, msg: TryBroadcastTransaction) -> Result<()> {
        let TryBroadcastTransaction { tx, kind } = msg;

        let result = self.client.transaction_broadcast(&tx);

        if let Err(electrum_client::Error::Protocol(ref value)) = result {
            let rpc_error = parse_rpc_protocol_error(value)
                .with_context(|| format!("Failed to parse electrum error response '{value:?}'"))?;

            if rpc_error.code == i64::from(RpcErrorCode::RpcVerifyAlreadyInChain) {
                let txid = tx.txid();
                tracing::trace!(
                    %txid, kind = %kind.name(), "Attempted to broadcast transaction that was already on-chain",
                );

                return Ok(());
            }

            // We do this check because electrum sometimes returns an RpcVerifyError when it should
            // be returning a RpcVerifyAlreadyInChain error,
            if rpc_error.code == i64::from(RpcErrorCode::RpcVerifyError)
                && rpc_error.message == "bad-txns-inputs-missingorspent"
            {
                if let Ok(tx) = self.client.transaction_get(&tx.txid()) {
                    let txid = tx.txid();
                    tracing::trace!(
                        %txid, kind = %kind.name(), "Attempted to broadcast transaction that was already on-chain",
                    );
                    return Ok(());
                }
            }
        }
        let txid = tx.txid();

        result.with_context(|| {
            let tx_hex = serialize_hex(&tx);

            format!("Failed to broadcast transaction. Txid: {txid}. Kind: {}. Raw transaction: {tx_hex}", kind.name())
        })?;

        tracing::info!(%txid, kind = %kind.name(), "Transaction published on chain");

        TRANSACTION_BROADCAST_COUNTER
            .with(&HashMap::from([(KIND_LABEL, kind.name())]))
            .inc();

        Ok(())
    }

    async fn handle_reinit_monitoring(&mut self, msg: ReinitMonitoring) {
        let ReinitMonitoring {
            id,
            lock,
            monitor_lock_finality,
            collaborative_settlement,
            monitor_collaborative_settlement_finality,
            commit,
            monitor_commit_finality,
            monitor_cet_timelock,
            monitor_refund_timelock,
            cet,
            monitor_cet_finality,
            refund,
            monitor_refund_finality,
            monitor_revoked_commit_transactions,
        } = msg;

        if let (Some(lock), true) = (lock, monitor_lock_finality) {
            self.monitor_lock_finality(id, lock);
        }

        if let Some(commit) = commit {
            if monitor_commit_finality {
                self.monitor_commit_finality(id, commit.clone());
            }

            if monitor_cet_timelock {
                self.monitor_commit_cet_timelock(id, commit.clone());
            }

            if let (Some(refund), true) = (&refund, monitor_refund_timelock) {
                self.monitor_commit_refund_timelock(id, commit, refund.timelock);
            }
        }

        if let (Some(refund), true) = (refund, monitor_refund_finality) {
            self.monitor_refund_finality(id, refund);
        }

        self.monitor_revoked_commit_transactions(id, monitor_revoked_commit_transactions);

        if let (Some(params), true) = (
            collaborative_settlement,
            monitor_collaborative_settlement_finality,
        ) {
            self.monitor_close_finality(id, params);
        }

        if let (Some(params), true) = (cet, monitor_cet_finality) {
            self.monitor_cet_finality(id, params);
        }
    }

    async fn handle_monitor_cet_finality(&mut self, msg: MonitorCetFinality) -> Result<()> {
        let txid = msg.cet.txid();
        let script = msg
            .cet
            .output
            .first()
            .context("Failed to monitor cet using script pubkey because no TxOut's in CET")?
            .script_pubkey
            .clone();

        self.monitor_cet_finality(msg.order_id, (txid, script));

        Ok(())
    }
}

impl MonitorAfterContractSetup {
    pub fn new(order_id: OrderId, dlc: &Dlc) -> Self {
        Self {
            order_id,
            transactions: TransactionsAfterContractSetup::new(dlc),
        }
    }
}

impl MonitorAfterRollover {
    pub fn new(order_id: OrderId, dlc: &Dlc) -> Self {
        Self {
            order_id,
            transactions: TransactionsAfterRollover::new(dlc),
        }
    }
}

struct TransactionsAfterContractSetup {
    lock: Lock,
    commit: Commit,
    refund: Refund,
}

impl TransactionsAfterContractSetup {
    pub fn new(dlc: &Dlc) -> Self {
        let (lock_tx, lock_descriptor) = &dlc.lock;

        let (commit_tx, _, commit_descriptor) = &dlc.commit;

        // We can assume that either one of the two addresses will be present since both parties
        // should have put up coins to create the CFD
        let refund_script_pubkey = dlc.maker_address.script_pubkey();
        let refund_txid = dlc.refund.0.txid();
        let refund_timelock = dlc.refund_timelock;

        Self {
            lock: Lock {
                txid: lock_tx.txid(),
                descriptor: lock_descriptor.clone(),
            },
            commit: Commit {
                txid: commit_tx.txid(),
                descriptor: commit_descriptor.clone(),
            },
            refund: Refund {
                txid: refund_txid,
                script_pubkey: refund_script_pubkey,
                timelock: refund_timelock,
            },
        }
    }
}

struct TransactionsAfterRollover {
    commit: Commit,
    refund: Refund,
    revoked_commits: Vec<RevokedCommit>,
}

impl TransactionsAfterRollover {
    pub fn new(dlc: &Dlc) -> Self {
        let (commit_tx, _, commit_descriptor) = &dlc.commit;

        // We can assume that either one of the two addresses will be present since both parties
        // should have put up coins to create the CFD
        let refund_script_pubkey = dlc.maker_address.script_pubkey();
        let refund_txid = dlc.refund.0.txid();
        let refund_timelock = dlc.refund_timelock;

        let revoked_commits = dlc
            .revoked_commit
            .iter()
            .map(
                |model::RevokedCommit {
                     txid,
                     script_pubkey,
                     ..
                 }| RevokedCommit {
                    txid: *txid,
                    script_pubkey: script_pubkey.clone(),
                },
            )
            .collect();

        Self {
            commit: Commit {
                txid: commit_tx.txid(),
                descriptor: commit_descriptor.clone(),
            },
            refund: Refund {
                txid: refund_txid,
                script_pubkey: refund_script_pubkey,
                timelock: refund_timelock,
            },
            revoked_commits,
        }
    }
}

#[derive(Clone)]
struct Lock {
    txid: Txid,
    descriptor: Descriptor<PublicKey>,
}

#[derive(Clone)]
struct Commit {
    txid: Txid,
    descriptor: Descriptor<PublicKey>,
}

#[derive(Clone)]
struct Refund {
    txid: Txid,
    script_pubkey: Script,
    timelock: u32,
}

#[derive(Clone)]
struct RevokedCommit {
    txid: Txid,
    script_pubkey: Script,
}

struct ReinitMonitoring {
    id: OrderId,

    lock: Option<Lock>,
    monitor_lock_finality: bool,

    collaborative_settlement: Option<(Txid, Script)>,
    monitor_collaborative_settlement_finality: bool,

    commit: Option<Commit>,
    monitor_commit_finality: bool,
    monitor_cet_timelock: bool,
    monitor_refund_timelock: bool,

    cet: Option<(Txid, Script)>,
    monitor_cet_finality: bool,

    refund: Option<Refund>,
    monitor_refund_finality: bool,

    monitor_revoked_commit_transactions: Vec<RevokedCommit>,
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: Sync) {
        if let Err(e) = self.sync().await {
            tracing::warn!("Sync failed: {:#}", e);
        }
    }
}

const KIND_LABEL: &str = "kind";

static TRANSACTION_BROADCAST_COUNTER: conquer_once::Lazy<prometheus::IntCounterVec> =
    conquer_once::Lazy::new(|| {
        prometheus::register_int_counter_vec!(
            "blockchain_transactions_broadcast_total",
            "The number of transactions broadcast.",
            &[KIND_LABEL]
        )
        .unwrap()
    });
