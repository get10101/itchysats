use crate::actors::log_error;
use crate::model::cfd::{CetStatus, Cfd, CfdState, Dlc, OrderId};
use crate::oracle;
use anyhow::{Context, Result};
use async_trait::async_trait;
use bdk::bitcoin::{PublicKey, Script, Txid};
use bdk::descriptor::Descriptor;
use bdk::electrum_client::{ElectrumApi, GetHistoryRes, HeaderNotification};
use bdk::miniscript::DescriptorTrait;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::convert::{TryFrom, TryInto};
use std::fmt;
use std::ops::{Add, RangeInclusive};

const FINALITY_CONFIRMATIONS: u32 = 1;

pub struct StartMonitoring {
    pub id: OrderId,
    pub params: MonitorParams,
}

#[derive(Clone)]
pub struct MonitorParams {
    lock: (Txid, Descriptor<PublicKey>),
    commit: (Txid, Descriptor<PublicKey>),
    cets: Vec<(Txid, Script, RangeInclusive<u64>)>,
    refund: (Txid, Script, u32),
}

pub struct Sync;

pub struct Actor<T>
where
    T: xtra::Actor,
{
    cfds: HashMap<OrderId, MonitorParams>,
    cfd_actor_addr: xtra::Address<T>,

    client: bdk::electrum_client::Client,
    latest_block_height: BlockHeight,
    current_status: BTreeMap<(Txid, Script), ScriptStatus>,
    awaiting_status: HashMap<(Txid, Script), Vec<(ScriptStatus, Event)>>,
}

impl<T> Actor<T>
where
    T: xtra::Actor + xtra::Handler<Event>,
{
    pub async fn new(
        electrum_rpc_url: &str,
        cfd_actor_addr: xtra::Address<T>,
        cfds: Vec<Cfd>,
    ) -> Result<Self> {
        let client = bdk::electrum_client::Client::new(electrum_rpc_url)
            .context("Failed to initialize Electrum RPC client")?;

        // Initially fetch the latest block for storing the height.
        // We do not act on this subscription after this call.
        let latest_block = client
            .block_headers_subscribe()
            .context("Failed to subscribe to header notifications")?;

        let mut actor = Self {
            cfds: HashMap::new(),
            cfd_actor_addr,
            client,
            latest_block_height: BlockHeight::try_from(latest_block)?,
            current_status: BTreeMap::default(),
            awaiting_status: HashMap::default(),
        };

        for cfd in cfds {
            match cfd.state.clone() {
                // In PendingOpen we know the complete dlc setup and assume that the lock transaction will be published
                CfdState::PendingOpen { dlc, .. } => {
                    let params = MonitorParams::from_dlc_and_timelocks(dlc.clone(), cfd.refund_timelock_in_blocks());
                    actor.cfds.insert(cfd.order.id, params.clone());
                    actor.monitor_all(&params, cfd.order.id);
                }
                CfdState::Open { dlc, .. } | CfdState::PendingCommit { dlc, .. } => {
                    let params = MonitorParams::from_dlc_and_timelocks(dlc.clone(), cfd.refund_timelock_in_blocks());
                    actor.cfds.insert(cfd.order.id, params.clone());

                    actor.monitor_commit_finality(&params, cfd.order.id);
                    actor.monitor_commit_cet_timelock(&params, cfd.order.id);
                    actor.monitor_commit_refund_timelock(&params, cfd.order.id);
                    actor.monitor_refund_finality(&params,cfd.order.id);
                }
                CfdState::OpenCommitted { dlc, cet_status, .. } => {
                    let params = MonitorParams::from_dlc_and_timelocks(dlc.clone(), cfd.refund_timelock_in_blocks());
                    actor.cfds.insert(cfd.order.id, params.clone());

                    match cet_status {
                        CetStatus::Unprepared
                        | CetStatus::OracleSigned(_) => {
                            actor.monitor_commit_cet_timelock(&params, cfd.order.id);
                            actor.monitor_commit_refund_timelock(&params, cfd.order.id);
                            actor.monitor_refund_finality(&params,cfd.order.id);
                        }
                        CetStatus::TimelockExpired => {
                            actor.monitor_commit_refund_timelock(&params, cfd.order.id);
                            actor.monitor_refund_finality(&params,cfd.order.id);
                        }
                        CetStatus::Ready(_price) => {
                            // TODO: monitor CET finality

                            actor.monitor_commit_refund_timelock(&params, cfd.order.id);
                            actor.monitor_refund_finality(&params,cfd.order.id);
                        }
                    }
                }
                CfdState::MustRefund { dlc, .. } => {
                    // TODO: CET monitoring (?) - note: would require to add CetStatus information to MustRefund

                    let params = MonitorParams::from_dlc_and_timelocks(dlc.clone(), cfd.refund_timelock_in_blocks());
                    actor.cfds.insert(cfd.order.id, params.clone());

                    actor.monitor_commit_refund_timelock(&params, cfd.order.id);
                    actor.monitor_refund_finality(&params,cfd.order.id);
                }

                // too early to monitor
                CfdState::OutgoingOrderRequest { .. }
                | CfdState::IncomingOrderRequest { .. }
                | CfdState::Accepted { .. }
                | CfdState::ContractSetup { .. }

                // final states
                | CfdState::Rejected { .. }
                | CfdState::Refunded { .. }
                | CfdState::SetupFailed { .. } => ()
            }
        }

        Ok(actor)
    }

    fn monitor_all(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.monitor_lock_finality(params, order_id);
        self.monitor_commit_finality(params, order_id);
        self.monitor_commit_cet_timelock(params, order_id);
        self.monitor_commit_refund_timelock(params, order_id);
        self.monitor_refund_finality(params, order_id);
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

    fn monitor_commit_cet_timelock(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.awaiting_status
            .entry((params.commit.0, params.commit.1.script_pubkey()))
            .or_default()
            .push((
                ScriptStatus::Confirmed(Confirmed::with_confirmations(Cfd::CET_TIMELOCK)),
                Event::CetTimelockExpired(order_id),
            ));
    }

    fn monitor_commit_refund_timelock(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.awaiting_status
            .entry((params.commit.0, params.commit.1.script_pubkey()))
            .or_default()
            .push((
                ScriptStatus::Confirmed(Confirmed::with_confirmations(params.refund.2)),
                Event::RefundTimelockExpired(order_id),
            ));
    }

    fn monitor_refund_finality(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.awaiting_status
            .entry((params.refund.0, params.refund.1.clone()))
            .or_default()
            .push((ScriptStatus::finality(), Event::RefundFinality(order_id)));
    }

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

        tracing::trace!(
            "Updating status of {} transactions",
            self.awaiting_status.len()
        );

        let histories = self
            .client
            .batch_script_get_history(self.awaiting_status.keys().map(|(_, script)| script))
            .context("Failed to get script histories")?;

        self.update_state(latest_block_height, histories).await?;

        Ok(())
    }

    async fn handle_oracle_attestation(&mut self, attestation: oracle::Attestation) -> Result<()> {
        for (order_id, MonitorParams { cets, .. }) in self.cfds.clone().into_iter() {
            let (txid, script_pubkey) =
                match cets.iter().find_map(|(txid, script_pubkey, range)| {
                    range
                        .contains(&attestation.price)
                        .then(|| (txid, script_pubkey))
                }) {
                    Some(cet) => cet,
                    None => continue,
                };

            self.awaiting_status
                .entry((*txid, script_pubkey.clone()))
                .or_default()
                .push((ScriptStatus::finality(), Event::CetFinality(order_id)));
        }

        Ok(())
    }

    async fn update_state(
        &mut self,
        latest_block_height: BlockHeight,
        histories: Vec<Vec<GetHistoryRes>>,
    ) -> Result<()> {
        if latest_block_height > self.latest_block_height {
            tracing::debug!(
                block_height = u32::from(latest_block_height),
                "Got notification for new block"
            );
            self.latest_block_height = latest_block_height;
        }

        // 1. shape response into local data format
        let new_status = histories.into_iter().zip(self.awaiting_status.keys().cloned()).map(|(script_history, (txid, script))| {
            let new_script_status = match script_history.as_slice() {
                [] => ScriptStatus::Unseen,
                [remaining @ .., last] => {
                    if !remaining.is_empty() {
                        tracing::warn!("Found more than a single history entry for script. This is highly unexpected and those history entries will be ignored")
                    }

                    if last.height <= 0 {
                        ScriptStatus::InMempool
                    } else {
                        ScriptStatus::Confirmed(
                            Confirmed::from_inclusion_and_latest_block(
                                u32::try_from(last.height).expect("we checked that height is > 0"),
                                u32::from(self.latest_block_height),
                            ),
                        )
                    }
                }
            };

            ((txid, script), new_script_status)
        }).collect::<BTreeMap<_, _>>();

        // 2. log any changes since our last sync
        for ((txid, script), status) in new_status.iter() {
            let old = self.current_status.get(&(*txid, script.clone()));

            print_status_change(*txid, old, status);
        }

        // 3. update local state
        self.current_status = new_status;

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
                                "{:?} requires {} and we have {}",
                                event,
                                target_status,
                                status
                            );

                            status >= target_status
                        });

                    tracing::trace!("{} subscriptions reached their monitoring target, {} remaining for this script", reached_monitoring_target.len(), remaining.len());

                    occupied.insert(remaining);

                    for (target_status, event) in reached_monitoring_target {
                        tracing::info!(%txid, target = %target_status, current = %status, "Bitcoin transaction reached monitoring target");
                        self.cfd_actor_addr.send(event).await?;
                    }
                }
            }
        }

        Ok(())
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
    fn finality() -> Self {
        Self::Confirmed(Confirmed::with_confirmations(FINALITY_CONFIRMATIONS))
    }
}

impl fmt::Display for ScriptStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScriptStatus::Unseen => write!(f, "unseen"),
            ScriptStatus::InMempool => write!(f, "in mempool"),
            ScriptStatus::Confirmed(inner) => {
                write!(f, "confirmed with {} blocks", inner.confirmations())
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

impl xtra::Message for StartMonitoring {
    type Result = ();
}

#[derive(Debug, Clone)]
pub enum Event {
    LockFinality(OrderId),
    CommitFinality(OrderId),
    CetTimelockExpired(OrderId),
    CetFinality(OrderId),
    RefundTimelockExpired(OrderId),
    RefundFinality(OrderId),
}

impl Event {
    pub fn order_id(&self) -> OrderId {
        let order_id = match self {
            Event::LockFinality(order_id) => order_id,
            Event::CommitFinality(order_id) => order_id,
            Event::CetTimelockExpired(order_id) => order_id,
            Event::RefundTimelockExpired(order_id) => order_id,
            Event::RefundFinality(order_id) => order_id,
            Event::CetFinality(order_id) => order_id,
        };

        *order_id
    }
}

impl MonitorParams {
    pub fn from_dlc_and_timelocks(dlc: Dlc, refund_timelock_in_blocks: u32) -> Self {
        let script_pubkey = dlc.address.script_pubkey();
        MonitorParams {
            lock: (dlc.lock.0.txid(), dlc.lock.1),
            commit: (dlc.commit.0.txid(), dlc.commit.2),
            cets: dlc
                .cets
                .into_iter()
                .map(|(tx, _, range)| (tx.txid(), script_pubkey.clone(), range))
                .collect(),
            refund: (
                dlc.refund.0.txid(),
                script_pubkey,
                refund_timelock_in_blocks,
            ),
        }
    }
}

impl xtra::Message for Event {
    type Result = ();
}

impl xtra::Message for Sync {
    type Result = ();
}

impl<T> xtra::Actor for Actor<T> where T: xtra::Actor {}

#[async_trait]
impl<T> xtra::Handler<StartMonitoring> for Actor<T>
where
    T: xtra::Actor + xtra::Handler<Event>,
{
    async fn handle(&mut self, msg: StartMonitoring, _ctx: &mut xtra::Context<Self>) {
        let StartMonitoring { id, params } = msg;

        self.monitor_all(&params, id);
        self.cfds.insert(id, params);
    }
}
#[async_trait]
impl<T> xtra::Handler<Sync> for Actor<T>
where
    T: xtra::Actor + xtra::Handler<Event>,
{
    async fn handle(&mut self, _: Sync, _ctx: &mut xtra::Context<Self>) {
        log_error!(self.sync());
    }
}

#[async_trait]
impl<T> xtra::Handler<oracle::Attestation> for Actor<T>
where
    T: xtra::Actor + xtra::Handler<Event>,
{
    async fn handle(&mut self, msg: oracle::Attestation, _ctx: &mut xtra::Context<Self>) {
        log_error!(self.handle_oracle_attestation(msg));
    }
}
