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
use bdk::miniscript::DescriptorTrait;
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
use std::time::Duration;
use tracing::Instrument;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

const LOCK_FINALITY_CONFIRMATIONS: u32 = 1;
const CLOSE_FINALITY_CONFIRMATIONS: u32 = 3;
const COMMIT_FINALITY_CONFIRMATIONS: u32 = 1;
const CET_FINALITY_CONFIRMATIONS: u32 = 3;
const REFUND_FINALITY_CONFIRMATIONS: u32 = 3;

pub struct StartMonitoring {
    pub id: OrderId,
    pub params: MonitorParams,
}

pub struct MonitorCollaborativeSettlement {
    pub order_id: OrderId,
    pub tx: (Txid, Script),
}

pub struct MonitorCetFinality {
    pub order_id: OrderId,
    pub cet: Transaction,
}

// TODO: The design of this struct causes a lot of marshalling und unmarshelling that is quite
// unnecessary. Should be taken apart so we can handle all cases individually!
#[derive(Clone)]
pub struct MonitorParams {
    lock: (Txid, Descriptor<PublicKey>),
    commit: (Txid, Descriptor<PublicKey>),
    refund: (Txid, Script, u32),
    revoked_commits: Vec<(Txid, Script)>,
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
    client: bdk::electrum_client::Client,
    state: State<Event>,
    db: sqlite_db::Connection,
}

/// Read-model of the CFD for the monitoring actor.
#[derive(Clone)]
struct Cfd {
    id: OrderId,
    params: Option<MonitorParams>,

    monitor_lock_finality: bool,
    monitor_commit_finality: bool,
    monitor_cet_timelock: bool,
    monitor_refund_timelock: bool,
    monitor_refund_finality: bool,
    monitor_revoked_commit_transactions: bool,

    // Ideally, all of the above would be like this.
    monitor_collaborative_settlement_finality: Option<(Txid, Script)>,
    monitor_cet_finality: Option<(Txid, Script)>,

    // Rebroadcast transactions upon startup
    lock_tx: Option<Transaction>,
    cet: Option<Transaction>,
    commit_tx: Option<Transaction>,

    version: u32,
}

impl sqlite_db::CfdAggregate for Cfd {
    type CtorArgs = ();

    fn new(_: Self::CtorArgs, cfd: sqlite_db::Cfd) -> Self {
        Self {
            id: cfd.id,
            params: None,
            monitor_lock_finality: false,
            monitor_commit_finality: false,
            monitor_cet_timelock: false,
            monitor_refund_timelock: false,
            monitor_refund_finality: false,
            monitor_revoked_commit_transactions: false,
            monitor_collaborative_settlement_finality: None,
            monitor_cet_finality: None,
            lock_tx: None,
            cet: None,
            commit_tx: None,
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
            ContractSetupCompleted { dlc, .. } => Self {
                params: dlc.clone().map(MonitorParams::new),
                monitor_lock_finality: true,
                monitor_commit_finality: true,
                monitor_cet_timelock: true,
                monitor_refund_timelock: true,
                monitor_refund_finality: true,
                monitor_revoked_commit_transactions: false,
                monitor_collaborative_settlement_finality: None,
                lock_tx: dlc.map(|dlc| dlc.lock.0),
                cet: None,
                commit_tx: None,
                ..self
            },
            RolloverCompleted { dlc, .. } => {
                Self {
                    params: dlc.map(MonitorParams::new),
                    monitor_lock_finality: false, // Lock is already final after rollover.
                    monitor_commit_finality: true,
                    monitor_cet_timelock: true,
                    monitor_refund_timelock: true,
                    monitor_refund_finality: true,
                    monitor_revoked_commit_transactions: true, /* After rollover, the other party
                                                                * might publish old states. */
                    monitor_collaborative_settlement_finality: None,
                    lock_tx: None,
                    cet: self.cet,
                    commit_tx: self.commit_tx,
                    ..self
                }
            }
            CollaborativeSettlementCompleted {
                spend_tx, script, ..
            } => {
                Self {
                    monitor_lock_finality: false, // Lock is already final if we collab settle.
                    lock_tx: None,
                    monitor_commit_finality: true, // The other party might still want to race us.
                    monitor_collaborative_settlement_finality: Some((spend_tx.txid(), script)),
                    ..self
                }
            }
            ContractSetupStarted | ContractSetupFailed | OfferRejected | RolloverRejected => Self {
                monitor_lock_finality: false,
                monitor_commit_finality: false,
                monitor_cet_timelock: false,
                monitor_refund_timelock: false,
                monitor_refund_finality: false,
                monitor_revoked_commit_transactions: false,
                monitor_collaborative_settlement_finality: None,
                lock_tx: None,
                cet: None,
                commit_tx: None,
                ..self
            },
            LockConfirmed | LockConfirmedAfterFinality => Self {
                monitor_lock_finality: false,
                lock_tx: None,
                ..self
            },
            CommitConfirmed => Self {
                monitor_commit_finality: false,
                commit_tx: None,
                ..self
            },
            // final states, don't monitor anything
            CetConfirmed | RefundConfirmed | CollaborativeSettlementConfirmed => Self {
                monitor_lock_finality: false,
                monitor_commit_finality: false,
                monitor_cet_timelock: false,
                monitor_refund_timelock: false,
                monitor_refund_finality: false,
                monitor_revoked_commit_transactions: false,
                monitor_collaborative_settlement_finality: None,
                monitor_cet_finality: None,
                lock_tx: None,
                cet: None,
                commit_tx: None,
                ..self
            },
            CetTimelockExpiredPriorOracleAttestation => Self {
                monitor_cet_timelock: false,
                ..self
            },
            CetTimelockExpiredPostOracleAttestation { cet, .. } => Self {
                cet: Some(cet.clone()),
                monitor_cet_finality: cet_txid_and_script(cet),
                monitor_cet_timelock: false,
                ..self
            },
            RefundTimelockExpired { .. } => Self {
                monitor_refund_timelock: false,
                ..self
            },
            OracleAttestedPostCetTimelock { cet, .. } => Self {
                cet: Some(cet.clone()),
                monitor_cet_finality: cet_txid_and_script(cet),
                ..self
            },
            RolloverStarted { .. }
            | RolloverAccepted
            | RolloverFailed
            | ManualCommit { .. }
            | OracleAttestedPriorCetTimelock { .. }
            | CollaborativeSettlementStarted { .. }
            | CollaborativeSettlementRejected
            | CollaborativeSettlementFailed
            | CollaborativeSettlementProposalAccepted => self,
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
            client,
            executor,
            state: State::new(latest_block),
            db,
        })
    }
}

impl Actor {
    fn monitor_lock_finality(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.state.monitor(
            params.lock.0,
            params.lock.1.script_pubkey(),
            ScriptStatus::with_confirmations(LOCK_FINALITY_CONFIRMATIONS),
            Event::LockFinality(order_id),
        )
    }

    fn monitor_commit_finality(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.state.monitor(
            params.commit.0,
            params.commit.1.script_pubkey(),
            ScriptStatus::with_confirmations(COMMIT_FINALITY_CONFIRMATIONS),
            Event::CommitFinality(order_id),
        )
    }

    fn monitor_close_finality(&mut self, close_params: (Txid, Script), order_id: OrderId) {
        self.state.monitor(
            close_params.0,
            close_params.1,
            ScriptStatus::with_confirmations(CLOSE_FINALITY_CONFIRMATIONS),
            Event::CloseFinality(order_id),
        );
    }

    fn monitor_cet_finality(&mut self, close_params: (Txid, Script), order_id: OrderId) {
        self.state.monitor(
            close_params.0,
            close_params.1,
            ScriptStatus::with_confirmations(CET_FINALITY_CONFIRMATIONS),
            Event::CetFinality(order_id),
        );
    }

    fn monitor_commit_cet_timelock(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.state.monitor(
            params.commit.0,
            params.commit.1.script_pubkey(),
            ScriptStatus::with_confirmations(CET_TIMELOCK),
            Event::CetTimelockExpired(order_id),
        );
    }

    fn monitor_commit_refund_timelock(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.state.monitor(
            params.commit.0,
            params.commit.1.script_pubkey(),
            ScriptStatus::with_confirmations(params.refund.2),
            Event::RefundTimelockExpired(order_id),
        );
    }

    fn monitor_refund_finality(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.state.monitor(
            params.refund.0,
            params.refund.1.clone(),
            ScriptStatus::with_confirmations(REFUND_FINALITY_CONFIRMATIONS),
            Event::RefundFinality(order_id),
        );
    }

    fn monitor_revoked_commit_transactions(&mut self, params: &MonitorParams, order_id: OrderId) {
        for revoked_commit_tx in params.revoked_commits.iter() {
            self.state.monitor(
                revoked_commit_tx.0,
                revoked_commit_tx.1.clone(),
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

        let histories = self
            .client
            .batch_script_get_history(self.state.monitoring_scripts())
            .context("Failed to get script histories")?;

        let mut ready_events = self.state.update(
            latest_block_height,
            histories
                .into_iter()
                .map(|list| {
                    list.into_iter()
                        .map(|response| TxStatus {
                            height: response.height,
                            tx_hash: response.tx_hash,
                        })
                        .collect()
                })
                .collect(),
        );

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

        Ok(())
    }

    async fn invoke_cfd_command(
        &self,
        id: OrderId,
        handler: impl FnOnce(model::Cfd) -> Result<Option<CfdEvent>>,
    ) {
        match self.executor.execute(id, handler).await {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(order_id = %id, "Failed to update state of CFD: {e:#}");
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

impl MonitorParams {
    pub fn new(dlc: Dlc) -> Self {
        // this is used for the refund transaction, and we can assume
        // that both addresses will be present since both parties
        // should have put up coins
        let script_pubkey = dlc.maker_address.script_pubkey();
        let lock_tx = dlc.lock.0;
        MonitorParams {
            lock: (lock_tx.txid(), dlc.lock.1),
            commit: (dlc.commit.0.txid(), dlc.commit.2),
            refund: (dlc.refund.0.txid(), script_pubkey, dlc.refund_timelock),
            revoked_commits: dlc
                .revoked_commit
                .iter()
                .map(|rev_commit| (rev_commit.txid, rev_commit.script_pubkey.clone()))
                .collect(),
        }
    }
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
                            cet,
                            commit_tx,
                            lock_tx,
                            id,
                            params,
                            monitor_lock_finality,
                            monitor_commit_finality,
                            monitor_cet_timelock,
                            monitor_refund_timelock,
                            monitor_refund_finality,
                            monitor_revoked_commit_transactions,
                            monitor_collaborative_settlement_finality,
                            monitor_cet_finality,
                            ..
                        } = match cfd {
                            Ok(cfd) => cfd,
                            Err(e) => {
                                tracing::warn!("Failed to load CFD from database: {e:#}");
                                continue;
                            }
                        };
                        if let Some(tx) = commit_tx {
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

                        if let Some(tx) = cet {
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

                        if let Some(tx) = lock_tx {
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

                        let params = match params {
                            None => continue,
                            Some(params) => params,
                        };

                        this.send(ReinitMonitoring {
                            id,
                            params,
                            monitor_lock_finality,
                            monitor_commit_finality,
                            monitor_cet_timelock,
                            monitor_refund_timelock,
                            monitor_refund_finality,
                            monitor_revoked_commit_transactions,
                            monitor_collaborative_settlement_finality,
                            monitor_cet_finality,
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
    async fn handle_start_monitoring(&mut self, msg: StartMonitoring) {
        let StartMonitoring { id, params } = msg;

        let params_argument = &params;
        let order_id = id;

        self.monitor_lock_finality(params_argument, order_id);
        self.monitor_commit_finality(params_argument, order_id);
        self.monitor_commit_cet_timelock(params_argument, order_id);
        self.monitor_commit_refund_timelock(params_argument, order_id);
        self.monitor_refund_finality(params_argument, order_id);
        self.monitor_revoked_commit_transactions(params_argument, order_id);
    }

    fn handle_collaborative_settlement(
        &mut self,
        collaborative_settlement: MonitorCollaborativeSettlement,
    ) {
        self.monitor_close_finality(
            collaborative_settlement.tx,
            collaborative_settlement.order_id,
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
            params,
            monitor_lock_finality,
            monitor_commit_finality,
            monitor_cet_timelock,
            monitor_refund_timelock,
            monitor_refund_finality,
            monitor_revoked_commit_transactions,
            monitor_collaborative_settlement_finality,
            monitor_cet_finality,
        } = msg;

        if monitor_lock_finality {
            self.monitor_lock_finality(&params, id);
        }

        if monitor_commit_finality {
            self.monitor_commit_finality(&params, id)
        }

        if monitor_cet_timelock {
            self.monitor_commit_cet_timelock(&params, id);
        }

        if monitor_refund_timelock {
            self.monitor_commit_refund_timelock(&params, id);
        }

        if monitor_refund_finality {
            self.monitor_refund_finality(&params, id);
        }

        if monitor_revoked_commit_transactions {
            self.monitor_revoked_commit_transactions(&params, id);
        }

        if let Some(params) = monitor_collaborative_settlement_finality {
            self.monitor_close_finality(params, id);
        }

        if let Some(params) = monitor_cet_finality {
            self.monitor_cet_finality(params, id);
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

        self.monitor_cet_finality((txid, script), msg.order_id);

        Ok(())
    }
}

// TODO: Re-model this by tearing apart `MonitorParams`.
struct ReinitMonitoring {
    id: OrderId,

    params: MonitorParams,

    monitor_lock_finality: bool,
    monitor_commit_finality: bool,
    monitor_cet_timelock: bool,
    monitor_refund_timelock: bool,
    monitor_refund_finality: bool,
    monitor_revoked_commit_transactions: bool,

    // Ideally, all of the above would be like this.
    monitor_collaborative_settlement_finality: Option<(Txid, Script)>,
    monitor_cet_finality: Option<(Txid, Script)>,
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
