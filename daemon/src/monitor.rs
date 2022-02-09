use crate::bitcoin::consensus::encode::serialize_hex;
use crate::bitcoin::Transaction;
use crate::command;
use crate::db;
use crate::model;
use crate::model::cfd;
use crate::model::cfd::CfdEvent;
use crate::model::cfd::Dlc;
use crate::model::cfd::OrderId;
use crate::model::cfd::CET_TIMELOCK;
use crate::model::BitMexPriceEventId;
use crate::oracle;
use crate::oracle::Attestation;
use crate::try_continue;
use crate::wallet::RpcErrorCode;
use crate::Tasks;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::Address;
use bdk::bitcoin::PublicKey;
use bdk::bitcoin::Script;
use bdk::bitcoin::Txid;
use bdk::descriptor::Descriptor;
use bdk::electrum_client;
use bdk::electrum_client::ElectrumApi;
use bdk::electrum_client::GetHistoryRes;
use bdk::electrum_client::HeaderNotification;
use bdk::miniscript::DescriptorTrait;
use serde_json::Value;
use sqlx::SqlitePool;
use std::collections::hash_map::Entry;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::convert::TryInto;
use std::fmt;
use std::ops::Add;
use std::ops::RangeInclusive;
use std::time::Duration;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

const FINALITY_CONFIRMATIONS: u32 = 1;

pub struct StartMonitoring {
    pub id: OrderId,
    pub params: MonitorParams,
}

pub struct CollaborativeSettlement {
    pub order_id: OrderId,
    pub tx: (Txid, Script),
}

// TODO: The design of this struct causes a lot of marshalling und unmarshelling that is quite
// unnecessary. Should be taken apart so we can handle all cases individually!
#[derive(Clone)]
pub struct MonitorParams {
    lock: (Txid, Descriptor<PublicKey>),
    commit: (Txid, Descriptor<PublicKey>),
    cets: HashMap<BitMexPriceEventId, Vec<Cet>>,
    refund: (Txid, Script, u32),
    revoked_commits: Vec<(Txid, Script)>,
    event_id: BitMexPriceEventId,
}

pub struct TryBroadcastTransaction {
    pub tx: Transaction,
    pub kind: TransactionKind,
}

pub enum TransactionKind {
    Lock,
    Commit,
    Refund,
    CollaborativeClose,
    Cet,
}

/// Formats a [`TransactionKind`] for use in log messages.
///
/// This implementations has two main features:
///
/// 1. It does not duplicate the "transaction" part for CET as "t" in CET already stands for
/// "transaction".
/// 2. It allows the caller to use the alternate sigil (`#`), to make the first
/// letter uppercase in case [`TransactionKind`] is used at the beginning of a log message.
impl fmt::Display for TransactionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            match self {
                TransactionKind::Lock => write!(f, "Lock transaction"),
                TransactionKind::Commit => write!(f, "Commit transaction"),
                TransactionKind::Refund => write!(f, "Refund transaction"),
                TransactionKind::CollaborativeClose => write!(f, "Collaborative close transaction"),
                TransactionKind::Cet => write!(f, "CET"),
            }
        } else {
            match self {
                TransactionKind::Lock => write!(f, "lock transaction"),
                TransactionKind::Commit => write!(f, "commit transaction"),
                TransactionKind::Refund => write!(f, "refund transaction"),
                TransactionKind::CollaborativeClose => write!(f, "collaborative close transaction"),
                TransactionKind::Cet => write!(f, "CET"),
            }
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

pub struct Sync;

// TODO: Send messages to the projection actor upon finality events so we send out updates.
//  -> Might as well just send out all events independent of sending to the cfd actor.
pub struct Actor {
    cfds: HashMap<OrderId, MonitorParams>,
    executor: command::Executor,
    client: bdk::electrum_client::Client,
    tasks: Tasks,
    state: State,
    db: sqlx::SqlitePool,
}

/// Internal data structure encapsulating the monitoring state without performing any IO.
struct State {
    latest_block_height: BlockHeight,
    current_status: BTreeMap<(Txid, Script), ScriptStatus>,
    awaiting_status: HashMap<(Txid, Script), Vec<(ScriptStatus, Event)>>,
}

impl State {
    fn new(latest_block_height: BlockHeight) -> Self {
        State {
            latest_block_height,
            current_status: BTreeMap::default(),
            awaiting_status: HashMap::default(),
        }
    }
}

/// Read-model of the CFD for the monitoring actor.
#[derive(Default)]
struct Cfd {
    params: Option<MonitorParams>,

    monitor_lock_finality: bool,
    monitor_commit_finality: bool,
    monitor_cet_timelock: bool,
    monitor_refund_timelock: bool,
    monitor_refund_finality: bool,
    monitor_revoked_commit_transactions: bool,

    // Ideally, all of the above would be like this.
    monitor_collaborative_settlement_finality: Option<(Txid, Script)>,

    // Rebroadcast transactions upon startup
    lock_tx: Option<Transaction>,
    cet: Option<Transaction>,
    commit_tx: Option<Transaction>,
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
    fn apply(self, event: cfd::Event) -> Self {
        use CfdEvent::*;
        match event.event {
            ContractSetupCompleted { dlc, .. } => Self {
                params: Some(MonitorParams::new(dlc.clone())),
                monitor_lock_finality: true,
                monitor_commit_finality: true,
                monitor_cet_timelock: true,
                monitor_refund_timelock: true,
                monitor_refund_finality: true,
                monitor_revoked_commit_transactions: false,
                monitor_collaborative_settlement_finality: None,
                lock_tx: Some(dlc.lock.0),
                cet: None,
                commit_tx: None,
            },
            RolloverCompleted { dlc, .. } => {
                Self {
                    params: Some(MonitorParams::new(dlc)),
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
            ContractSetupStarted | ContractSetupFailed | OfferRejected | RolloverRejected => {
                Self::default() // all false / empty
            }
            LockConfirmed => Self {
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
            CetConfirmed
            | RefundConfirmed
            | CollaborativeSettlementConfirmed
            | LockConfirmedAfterFinality => Self::default(),
            CetTimelockExpiredPriorOracleAttestation => Self {
                monitor_cet_timelock: false,
                ..self
            },
            CetTimelockExpiredPostOracleAttestation { cet, .. } => Self {
                cet: Some(cet),
                monitor_cet_timelock: false,
                ..self
            },
            RefundTimelockExpired { .. } => Self {
                monitor_refund_timelock: false,
                ..self
            },
            OracleAttestedPostCetTimelock { cet, .. } => Self {
                cet: Some(cet),
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
                tracing::error!("Revoked logic not implemented");
                self
            }
        }
    }
}

impl Actor {
    pub fn new(
        db: SqlitePool,
        electrum_rpc_url: String,
        executor: command::Executor,
    ) -> Result<Self> {
        let client = bdk::electrum_client::Client::new(&electrum_rpc_url)
            .context("Failed to initialize Electrum RPC client")?;

        // Initially fetch the latest block for storing the height.
        // We do not act on this subscription after this call.
        let latest_block = client
            .block_headers_subscribe()
            .context("Failed to subscribe to header notifications")?;

        Ok(Self {
            cfds: HashMap::new(),
            client,
            executor,
            state: State::new(BlockHeight::try_from(latest_block)?),
            tasks: Tasks::default(),
            db,
        })
    }
}

impl State {
    fn monitor_all(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.monitor_lock_finality(params, order_id);
        self.monitor_commit_finality(params, order_id);
        self.monitor_commit_cet_timelock(params, order_id);
        self.monitor_commit_refund_timelock(params, order_id);
        self.monitor_refund_finality(params, order_id);
        self.monitor_revoked_commit_transactions(params, order_id);
    }

    fn monitor_lock_finality(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.awaiting_status
            .entry((params.lock.0, params.lock.1.script_pubkey()))
            .or_default()
            .push((ScriptStatus::finality(), Event::LockFinality(order_id)));
    }

    fn monitor_commit_finality(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.awaiting_status
            .entry((params.commit.0, params.commit.1.script_pubkey()))
            .or_default()
            .push((ScriptStatus::finality(), Event::CommitFinality(order_id)));
    }

    fn monitor_close_finality(&mut self, close_params: (Txid, Script), order_id: OrderId) {
        self.awaiting_status
            .entry(close_params)
            .or_default()
            .push((ScriptStatus::finality(), Event::CloseFinality(order_id)));
    }

    fn monitor_commit_cet_timelock(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.awaiting_status
            .entry((params.commit.0, params.commit.1.script_pubkey()))
            .or_default()
            .push((
                ScriptStatus::with_confirmations(CET_TIMELOCK),
                Event::CetTimelockExpired(order_id),
            ));
    }

    fn monitor_commit_refund_timelock(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.awaiting_status
            .entry((params.commit.0, params.commit.1.script_pubkey()))
            .or_default()
            .push((
                ScriptStatus::with_confirmations(params.refund.2),
                Event::RefundTimelockExpired(order_id),
            ));
    }

    fn monitor_refund_finality(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.awaiting_status
            .entry((params.refund.0, params.refund.1.clone()))
            .or_default()
            .push((ScriptStatus::finality(), Event::RefundFinality(order_id)));
    }

    fn monitor_cet_finality(
        &mut self,
        cets: HashMap<BitMexPriceEventId, Vec<Cet>>,
        attestation: Attestation,
        order_id: OrderId,
    ) -> Result<()> {
        let attestation_id = attestation.id;
        let cets = cets.get(&attestation_id).with_context(|| {
            format!("No CET for oracle event {attestation_id} and cfd with order id {order_id}",)
        })?;

        let (txid, script_pubkey) = cets
            .iter()
            .find_map(
                |Cet {
                     txid,
                     script,
                     range,
                     ..
                 }| { range.contains(&attestation.price).then(|| (txid, script)) },
            )
            .context("No price range match for oracle attestation")?;

        self.awaiting_status
            .entry((*txid, script_pubkey.clone()))
            .or_default()
            .push((ScriptStatus::finality(), Event::CetFinality(order_id)));

        Ok(())
    }

    fn monitor_revoked_commit_transactions(&mut self, params: &MonitorParams, order_id: OrderId) {
        for revoked_commit_tx in params.revoked_commits.iter() {
            self.awaiting_status
                .entry((revoked_commit_tx.0, revoked_commit_tx.1.clone()))
                .or_default()
                .push((
                    ScriptStatus::InMempool,
                    Event::RevokedTransactionFound(order_id),
                ));
        }
    }
}

impl Actor {
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
            .try_into()?;

        let num_transactions = self.state.awaiting_status.len();

        tracing::trace!("Updating status of {num_transactions} transactions",);

        let histories = self
            .client
            .batch_script_get_history(self.state.awaiting_status.keys().map(|(_, script)| script))
            .context("Failed to get script histories")?;

        let mut ready_events = self.state.update(latest_block_height, histories);

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
        handler: impl FnOnce(model::cfd::Cfd) -> Result<Option<model::cfd::Event>>,
    ) {
        match self.executor.execute(id, handler).await {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(order_id = %id, "Failed to update state of CFD: {e:#}");
            }
        }
    }

    async fn handle_oracle_attestation(&mut self, attestation: oracle::Attestation) {
        for (order_id, MonitorParams { cets, .. }) in self
            .cfds
            .clone()
            .into_iter()
            .filter(|(_, params)| params.event_id == attestation.id)
        {
            try_continue!(self
                .state
                .monitor_cet_finality(cets, attestation.clone(), order_id))
        }
    }
}

impl State {
    fn update(
        &mut self,
        latest_block_height: BlockHeight,
        response: Vec<Vec<GetHistoryRes>>,
    ) -> Vec<Event> {
        let txid_to_script = self
            .awaiting_status
            .keys()
            .cloned()
            .collect::<HashMap<_, _>>();

        let mut histories = HashMap::new();
        for history in response {
            for response in history {
                let txid = response.tx_hash;
                let script = match txid_to_script.get(&txid) {
                    None => {
                        tracing::trace!(
                            "Could not find script in own state for txid {txid}, ignoring"
                        );
                        continue;
                    }
                    Some(script) => script,
                };
                histories.insert((txid, script.clone()), response);
            }
        }

        if latest_block_height > self.latest_block_height {
            tracing::debug!(
                block_height = u32::from(latest_block_height),
                "Got notification for new block"
            );
            self.latest_block_height = latest_block_height;
        }

        // 1. Decide new status based on script history
        let new_status = self
            .awaiting_status
            .iter()
            .map(|(key, _old_status)| {
                let new_script_status = match histories.get(key) {
                    None => ScriptStatus::Unseen,
                    Some(history_entry) => {
                        if history_entry.height <= 0 {
                            ScriptStatus::InMempool
                        } else {
                            ScriptStatus::Confirmed(Confirmed::from_inclusion_and_latest_block(
                                u32::try_from(history_entry.height)
                                    .expect("we checked that height is > 0"),
                                u32::from(self.latest_block_height),
                            ))
                        }
                    }
                };

                (key.clone(), new_script_status)
            })
            .collect::<BTreeMap<_, _>>();

        // 2. log any changes since our last sync
        for ((txid, script), status) in new_status.iter() {
            let old = self.current_status.get(&(*txid, script.clone()));

            print_status_change(*txid, old, status);
        }

        // 3. update local state
        self.current_status = new_status;

        let mut ready_events = Vec::new();

        // 4. check for finished monitoring tasks
        for ((txid, script), status) in self.current_status.iter() {
            match self.awaiting_status.entry((*txid, script.clone())) {
                Entry::Vacant(_) => {
                    unreachable!("we are only fetching the status of scripts we are waiting for")
                }
                Entry::Occupied(mut occupied) => {
                    let targets = occupied.insert(Vec::new());

                    // Split vec into two lists, all the ones for which we reached the target and
                    // the ones which we need to still monitor
                    let (reached_monitoring_target, remaining) = targets
                        .into_iter()
                        .partition::<Vec<_>, _>(|(target_status, event)| {
                            tracing::trace!(
                                "{event:?} requires {target_status} and we have {status}"
                            );

                            status >= target_status
                        });

                    let num_reached = reached_monitoring_target.len();
                    let num_remaining = remaining.len();
                    tracing::trace!("{num_reached} subscriptions reached their monitoring target, {num_remaining} remaining for this script");

                    // TODO: When reaching finality of a final tx (CET, refund_tx,
                    // collaborate_close_tx) we have to remove the remaining "competing"
                    // transactions. This is not critical, but when fetching
                    // `GetHistoryRes` by script we can have entries that we don't care about
                    // anymore.

                    if remaining.is_empty() {
                        occupied.remove();
                    } else {
                        occupied.insert(remaining);
                    }

                    for (target_status, event) in reached_monitoring_target {
                        tracing::info!(%txid, target = %target_status, current = %status, "Bitcoin transaction reached monitoring target");
                        ready_events.push(event);
                    }
                }
            }
        }

        ready_events
    }
}

fn print_status_change(txid: Txid, old: Option<&ScriptStatus>, new: &ScriptStatus) {
    match (old, new) {
        (None, new_status) if new_status > &ScriptStatus::Unseen => {
            tracing::debug!(%txid, status = %new_status, "Found relevant Bitcoin transaction");
        }
        (Some(old_status), new_status) if old_status != new_status => {
            tracing::debug!(%txid, %new_status, %old_status, "Bitcoin transaction status changed");
        }
        _ => {}
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd)]
struct Confirmed {
    /// The depth of this transaction within the blockchain.
    ///
    /// Will be zero if the transaction is included in the latest block.
    depth: u32,
}

impl Confirmed {
    fn with_confirmations(blocks: u32) -> Self {
        Self { depth: blocks - 1 }
    }

    /// Compute the depth of a transaction based on its inclusion height and the
    /// latest known block.
    ///
    /// Our information about the latest block might be outdated. To avoid an
    /// overflow, we make sure the depth is 0 in case the inclusion height
    /// exceeds our latest known block,
    fn from_inclusion_and_latest_block(inclusion_height: u32, latest_block: u32) -> Self {
        let depth = latest_block.saturating_sub(inclusion_height);

        Self { depth }
    }

    fn confirmations(&self) -> u32 {
        self.depth + 1
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd)]
enum ScriptStatus {
    Unseen,
    InMempool,
    Confirmed(Confirmed),
}

impl ScriptStatus {
    fn with_confirmations(confirmations: u32) -> Self {
        Self::Confirmed(Confirmed::with_confirmations(confirmations))
    }

    fn finality() -> Self {
        Self::with_confirmations(FINALITY_CONFIRMATIONS)
    }
}

impl fmt::Display for ScriptStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScriptStatus::Unseen => write!(f, "unseen"),
            ScriptStatus::InMempool => write!(f, "in mempool"),
            ScriptStatus::Confirmed(inner) => {
                let num_blocks = inner.confirmations();

                write!(f, "confirmed with {num_blocks} blocks",)
            }
        }
    }
}

/// Represent a block height, or block number, expressed in absolute block
/// count. E.g. The transaction was included in block #655123, 655123 block
/// after the genesis block.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Ord, PartialOrd)]
struct BlockHeight(u32);

impl From<BlockHeight> for u32 {
    fn from(height: BlockHeight) -> Self {
        height.0
    }
}

impl TryFrom<HeaderNotification> for BlockHeight {
    type Error = anyhow::Error;

    fn try_from(value: HeaderNotification) -> Result<Self, Self::Error> {
        Ok(Self(
            value
                .height
                .try_into()
                .context("Failed to fit usize into u32")?,
        ))
    }
}

impl Add<u32> for BlockHeight {
    type Output = BlockHeight;
    fn add(self, rhs: u32) -> Self::Output {
        BlockHeight(self.0 + rhs)
    }
}

#[derive(Debug, Clone, PartialEq)]
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
        let script_pubkey = dlc.maker_address.script_pubkey();
        MonitorParams {
            lock: (dlc.lock.0.txid(), dlc.lock.1),
            commit: (dlc.commit.0.txid(), dlc.commit.2),
            cets: map_cets(dlc.cets, &dlc.maker_address),
            refund: (dlc.refund.0.txid(), script_pubkey, dlc.refund_timelock),
            revoked_commits: dlc
                .revoked_commit
                .iter()
                .map(|rev_commit| (rev_commit.txid, rev_commit.script_pubkey.clone()))
                .collect(),
            event_id: dlc.settlement_event_id,
        }
    }
}

#[derive(Clone)]
struct Cet {
    txid: Txid,
    script: Script,
    range: RangeInclusive<u64>,
}

fn map_cets(
    cets: HashMap<BitMexPriceEventId, Vec<model::cfd::Cet>>,
    maker_address: &Address,
) -> HashMap<BitMexPriceEventId, Vec<Cet>> {
    cets.into_iter()
        .map(|(event_id, cets)| {
            (
                event_id,
                cets.into_iter()
                    .map(|cet| Cet {
                        txid: cet.txid,
                        script: maker_address.script_pubkey(),
                        range: cet.range,
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

impl xtra::Message for Sync {
    type Result = ();
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");
        self.tasks
            .add(this.clone().send_interval(Duration::from_secs(20), || Sync));

        self.tasks.add_fallible(
            {
                let db = self.db.clone();
                let this = this.clone();

                async move {
                    let mut conn = db.acquire().await?;

                    for id in db::load_all_cfd_ids(&mut conn).await? {
                        let (_, events) = db::load_cfd(id, &mut conn).await?;

                        let Cfd {
                            cet,
                            commit_tx,
                            lock_tx,
                            ..
                        } = events.into_iter().fold(Cfd::default(), Cfd::apply);

                        if let Some(tx) = commit_tx {
                            if let Err(e) = this
                                .send(TryBroadcastTransaction {
                                    tx,
                                    kind: TransactionKind::Commit,
                                })
                                .await?
                            {
                                tracing::warn!("{e:#}")
                            }
                        }

                        if let Some(tx) = cet {
                            if let Err(e) = this
                                .send(TryBroadcastTransaction {
                                    tx,
                                    kind: TransactionKind::Cet,
                                })
                                .await?
                            {
                                tracing::warn!("{e:#}")
                            }
                        }

                        if let Some(tx) = lock_tx {
                            if let Err(e) = this
                                .send(TryBroadcastTransaction {
                                    tx,
                                    kind: TransactionKind::Lock,
                                })
                                .await?
                            {
                                tracing::warn!("{e:#}")
                            }
                        }
                    }

                    anyhow::Ok(())
                }
            },
            |e| async move {
                tracing::warn!("Failed to re-broadcast transactions: {e:#}");
            },
        );

        self.tasks.add_fallible(
            {
                let db = self.db.clone();

                async move {
                    let mut conn = db.acquire().await?;

                    for id in db::load_all_cfd_ids(&mut conn).await? {
                        let (_, events) = db::load_cfd(id, &mut conn).await?;

                        let Cfd {
                            params,
                            monitor_lock_finality,
                            monitor_commit_finality,
                            monitor_cet_timelock,
                            monitor_refund_timelock,
                            monitor_refund_finality,
                            monitor_revoked_commit_transactions,
                            monitor_collaborative_settlement_finality,
                            ..
                        } = events.into_iter().fold(Cfd::default(), Cfd::apply);

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
    async fn handle_start_monitoring(
        &mut self,
        msg: StartMonitoring,
        _ctx: &mut xtra::Context<Self>,
    ) {
        let StartMonitoring { id, params } = msg;

        self.state.monitor_all(&params, id);
        self.cfds.insert(id, params);
    }

    fn handle_collaborative_settlement(
        &mut self,
        collaborative_settlement: CollaborativeSettlement,
    ) {
        self.state.monitor_close_finality(
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
                    %txid, "Attempted to broadcast {kind} that was already on-chain",
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
                        %txid, "Attempted to broadcast {kind} that was already on-chain",
                    );
                    return Ok(());
                }
            }
        }
        let txid = tx.txid();

        result.with_context(|| {
            let tx_hex = serialize_hex(&tx);

            format!("Broadcasting {kind} failed. Txid: {txid}. Raw transaction: {tx_hex}")
        })?;

        tracing::info!(%txid, "{kind:#} published on chain");

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
        } = msg;

        self.cfds.insert(id, params.clone());

        if monitor_lock_finality {
            self.state.monitor_lock_finality(&params, id);
        }

        if monitor_commit_finality {
            self.state.monitor_commit_finality(&params, id)
        }

        if monitor_cet_timelock {
            self.state.monitor_commit_cet_timelock(&params, id);
        }

        if monitor_refund_timelock {
            self.state.monitor_commit_refund_timelock(&params, id);
        }

        if monitor_refund_finality {
            self.state.monitor_refund_finality(&params, id);
        }

        if monitor_revoked_commit_transactions {
            self.state.monitor_revoked_commit_transactions(&params, id);
        }

        if let Some(params) = monitor_collaborative_settlement_finality {
            self.state.monitor_close_finality(params, id);
        }
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
}

#[async_trait]
impl xtra::Handler<Sync> for Actor {
    async fn handle(&mut self, _: Sync, _ctx: &mut xtra::Context<Self>) {
        if let Err(e) = self.sync().await {
            tracing::warn!("Sync failed: {:#}", e);
        }
    }
}

#[async_trait]
impl xtra::Handler<oracle::Attestation> for Actor {
    async fn handle(&mut self, msg: oracle::Attestation, _ctx: &mut xtra::Context<Self>) {
        self.handle_oracle_attestation(msg).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::cfd::CET_TIMELOCK;
    use tracing_subscriber::prelude::*;

    #[tokio::test]
    async fn can_handle_multiple_subscriptions_on_the_same_transaction() {
        let _guard = tracing_subscriber::fmt()
            .with_env_filter("trace")
            .with_test_writer()
            .set_default();

        let commit_finality = Event::CommitFinality(OrderId::default());
        let refund_expired = Event::RefundTimelockExpired(OrderId::default());

        let mut state = State::new(BlockHeight(0));
        state.awaiting_status = HashMap::from_iter([(
            (txid1(), script1()),
            vec![
                (ScriptStatus::finality(), commit_finality.clone()),
                (
                    ScriptStatus::with_confirmations(CET_TIMELOCK),
                    refund_expired.clone(),
                ),
            ],
        )]);

        let ready_events = state.update(
            BlockHeight(10),
            vec![vec![GetHistoryRes {
                height: 5,
                tx_hash: txid1(),
                fee: None,
            }]],
        );

        assert_eq!(ready_events, vec![commit_finality]);

        let ready_events = state.update(
            BlockHeight(20),
            vec![vec![GetHistoryRes {
                height: 5,
                tx_hash: txid1(),
                fee: None,
            }]],
        );

        assert_eq!(ready_events, vec![refund_expired]);
    }

    #[tokio::test]
    async fn update_for_a_script_only_results_in_event_for_corresponding_transaction() {
        let _guard = tracing_subscriber::fmt()
            .with_env_filter("trace")
            .with_test_writer()
            .set_default();

        let cet_finality = Event::CetFinality(OrderId::default());
        let refund_finality = Event::RefundFinality(OrderId::default());

        let mut state = State::new(BlockHeight(0));
        state.awaiting_status = HashMap::from_iter([
            (
                (txid1(), script1()),
                vec![(ScriptStatus::finality(), cet_finality.clone())],
            ),
            (
                (txid2(), script1()),
                vec![(ScriptStatus::finality(), refund_finality)],
            ),
        ]);

        let ready_events = state.update(
            BlockHeight(0),
            vec![vec![GetHistoryRes {
                height: 5,
                tx_hash: txid1(),
                fee: None,
            }]],
        );

        assert_eq!(ready_events, vec![cet_finality]);
    }

    #[tokio::test]
    async fn stop_monitoring_after_target_reached() {
        let _guard = tracing_subscriber::fmt()
            .with_env_filter("trace")
            .with_test_writer()
            .set_default();

        let cet_finality = Event::CetFinality(OrderId::default());

        let mut state = State::new(BlockHeight(0));
        state.awaiting_status = HashMap::from_iter([(
            (txid1(), script1()),
            vec![(ScriptStatus::finality(), cet_finality.clone())],
        )]);

        let ready_events = state.update(
            BlockHeight(0),
            vec![vec![GetHistoryRes {
                height: 5,
                tx_hash: txid1(),
                fee: None,
            }]],
        );

        assert_eq!(ready_events, vec![cet_finality]);
        assert!(state.awaiting_status.is_empty());
    }

    fn txid1() -> Txid {
        "1278ef8104c2f63c03d4d52bace29bed28bd5e664e67543735ddc95a39bfdc0f"
            .parse()
            .unwrap()
    }

    fn txid2() -> Txid {
        "07ade6a49e34ad4cc3ca3f79d78df462685f8f1fbc8a9b05af51ec503ea5b960"
            .parse()
            .unwrap()
    }

    fn script1() -> Script {
        "6a4c50001d97ca0002d3829148f63cc8ee21241e3f1c5eaee58781dd45a7d814710fac571b92aadff583e85d5a295f61856f469b401efe615657bf040c32f1000065bce011a420ca9ea3657fff154d95d1a95c".parse().unwrap()
    }
}
