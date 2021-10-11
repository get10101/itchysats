use crate::actors::log_error;
use crate::model::cfd::{CetStatus, Cfd, CfdState, Dlc, OrderId};
use crate::model::OracleEventId;
use crate::oracle::Attestation;
use crate::{model, oracle};
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
use std::marker::Send;
use std::ops::{Add, RangeInclusive};

const FINALITY_CONFIRMATIONS: u32 = 1;

pub struct StartMonitoring {
    pub id: OrderId,
    pub params: MonitorParams,
}

#[derive(Clone)]
pub struct Cet {
    txid: Txid,
    script: Script,
    range: RangeInclusive<u64>,
    n_bits: usize,
}

#[derive(Clone)]
pub struct MonitorParams {
    lock: (Txid, Descriptor<PublicKey>),
    commit: (Txid, Descriptor<PublicKey>),
    cets: HashMap<OracleEventId, Vec<Cet>>,
    refund: (Txid, Script, u32),
    revoked_commits: Vec<(Txid, Script)>,
}

pub struct Sync;

pub struct Actor<T, C = bdk::electrum_client::Client>
where
    T: xtra::Actor,
{
    cfds: HashMap<OrderId, MonitorParams>,
    cfd_actor_addr: xtra::Address<T>,

    client: C,
    latest_block_height: BlockHeight,
    current_status: BTreeMap<(Txid, Script), ScriptStatus>,
    awaiting_status: HashMap<(Txid, Script), Vec<(ScriptStatus, Event)>>,
}

impl<T> Actor<T, bdk::electrum_client::Client>
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
                CfdState::OpenCommitted { dlc, cet_status, .. }
                | CfdState::PendingCet { dlc, cet_status, .. } => {
                    let params = MonitorParams::from_dlc_and_timelocks(dlc.clone(), cfd.refund_timelock_in_blocks());
                    actor.cfds.insert(cfd.order.id, params.clone());

                    match cet_status {
                        CetStatus::Unprepared => {
                            actor.monitor_commit_cet_timelock(&params, cfd.order.id);
                            actor.monitor_commit_refund_timelock(&params, cfd.order.id);
                            actor.monitor_refund_finality(&params,cfd.order.id);
                        }
                        CetStatus::OracleSigned(attestation) => {
                            actor.monitor_cet_finality(map_cets(dlc.cets, dlc.maker_address.script_pubkey()), attestation.into(), cfd.order.id)?;
                            actor.monitor_commit_cet_timelock(&params, cfd.order.id);
                            actor.monitor_commit_refund_timelock(&params, cfd.order.id);
                            actor.monitor_refund_finality(&params,cfd.order.id);
                        }
                        CetStatus::TimelockExpired => {
                            actor.monitor_commit_refund_timelock(&params, cfd.order.id);
                            actor.monitor_refund_finality(&params,cfd.order.id);
                        }
                        CetStatus::Ready(attestation) => {
                            actor.monitor_cet_finality(map_cets(dlc.cets, dlc.maker_address.script_pubkey()), attestation.into(), cfd.order.id)?;
                            actor.monitor_commit_refund_timelock(&params, cfd.order.id);
                            actor.monitor_refund_finality(&params,cfd.order.id);
                        }
                    }
                }
                CfdState::MustRefund { dlc, .. } => {
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
                | CfdState::Closed { .. }
                | CfdState::Rejected { .. }
                | CfdState::Refunded { .. }
                | CfdState::SetupFailed { .. } => ()
            }
        }

        Ok(actor)
    }
}

impl<T, C> Actor<T, C>
where
    T: xtra::Actor + xtra::Handler<Event>,
    C: bdk::electrum_client::ElectrumApi,
{
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

    fn monitor_commit_cet_timelock(&mut self, params: &MonitorParams, order_id: OrderId) {
        self.awaiting_status
            .entry((params.commit.0, params.commit.1.script_pubkey()))
            .or_default()
            .push((
                ScriptStatus::with_confirmations(Cfd::CET_TIMELOCK),
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
        cets: HashMap<OracleEventId, Vec<Cet>>,
        attestation: Attestation,
        order_id: OrderId,
    ) -> Result<()> {
        let cets = cets
            .get(&attestation.id)
            .context("No CET for oracle event found")?;

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

        let txid_to_script = self
            .awaiting_status
            .keys()
            .cloned()
            .collect::<HashMap<_, _>>();

        let histories = self
            .client
            .batch_script_get_history(self.awaiting_status.keys().map(|(_, script)| script))
            .context("Failed to get script histories")?;

        let mut histories_grouped_by_txid = HashMap::new();
        for history in histories {
            for response in history {
                let txid = response.tx_hash;
                let script = match txid_to_script.get(&txid) {
                    None => {
                        tracing::error!(
                            "Could not find script in own state for txid {}, ignoring",
                            txid
                        );
                        continue;
                    }
                    Some(script) => script,
                };
                histories_grouped_by_txid.insert((txid, script.clone()), response);
            }
        }

        self.update_state(latest_block_height, histories_grouped_by_txid)
            .await?;

        Ok(())
    }

    async fn handle_oracle_attestation(&mut self, attestation: oracle::Attestation) -> Result<()> {
        for (order_id, MonitorParams { cets, .. }) in self.cfds.clone().into_iter() {
            self.monitor_cet_finality(cets, attestation.clone(), order_id)?;
        }

        Ok(())
    }

    async fn update_state(
        &mut self,
        latest_block_height: BlockHeight,
        histories: HashMap<(Txid, Script), GetHistoryRes>,
    ) -> Result<()> {
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

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    LockFinality(OrderId),
    CommitFinality(OrderId),
    CetTimelockExpired(OrderId),
    CetFinality(OrderId),
    RefundTimelockExpired(OrderId),
    RefundFinality(OrderId),
    RevokedTransactionFound(OrderId),
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
            Event::RevokedTransactionFound(order_id) => order_id,
        };

        *order_id
    }
}

impl MonitorParams {
    pub fn from_dlc_and_timelocks(dlc: Dlc, refund_timelock_in_blocks: u32) -> Self {
        let script_pubkey = dlc.maker_address.script_pubkey();
        MonitorParams {
            lock: (dlc.lock.0.txid(), dlc.lock.1),
            commit: (dlc.commit.0.txid(), dlc.commit.2),
            cets: map_cets(dlc.cets, script_pubkey.clone()),
            refund: (
                dlc.refund.0.txid(),
                script_pubkey,
                refund_timelock_in_blocks,
            ),
            revoked_commits: dlc
                .revoked_commit
                .iter()
                .map(|rev_commit| (rev_commit.txid, rev_commit.script_pubkey.clone()))
                .collect(),
        }
    }
}

fn map_cets(
    cets: HashMap<OracleEventId, Vec<model::cfd::Cet>>,
    script_pubkey: Script,
) -> HashMap<OracleEventId, Vec<Cet>> {
    cets.iter()
        .map(|(event_id, cets)| {
            (
                event_id.clone(),
                cets.iter()
                    .map(
                        |model::cfd::Cet {
                             tx, range, n_bits, ..
                         }| Cet {
                            txid: tx.txid(),
                            script: script_pubkey.clone(),
                            range: range.clone(),
                            n_bits: *n_bits,
                        },
                    )
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

impl xtra::Message for Event {
    type Result = ();
}

impl xtra::Message for Sync {
    type Result = ();
}

impl<T, C> xtra::Actor for Actor<T, C>
where
    T: xtra::Actor,
    C: Send,
    C: 'static,
{
}

#[async_trait]
impl<T, C> xtra::Handler<StartMonitoring> for Actor<T, C>
where
    T: xtra::Actor + xtra::Handler<Event>,
    C: bdk::electrum_client::ElectrumApi + Send + 'static,
{
    async fn handle(&mut self, msg: StartMonitoring, _ctx: &mut xtra::Context<Self>) {
        let StartMonitoring { id, params } = msg;

        self.monitor_all(&params, id);
        self.cfds.insert(id, params);
    }
}
#[async_trait]
impl<T, C> xtra::Handler<Sync> for Actor<T, C>
where
    T: xtra::Actor + xtra::Handler<Event>,
    C: bdk::electrum_client::ElectrumApi + Send + 'static,
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

#[cfg(test)]
mod tests {
    use super::*;
    use bdk::bitcoin::blockdata::block;
    use bdk::electrum_client::{
        Batch, Error, GetBalanceRes, GetHeadersRes, GetMerkleRes, ListUnspentRes,
        RawHeaderNotification, ServerFeaturesRes,
    };
    use std::iter::FromIterator;
    use tracing_subscriber::prelude::*;

    #[tokio::test]
    async fn can_handle_multiple_subscriptions_on_the_same_transaction() {
        let _guard = tracing_subscriber::fmt()
            .with_env_filter("trace")
            .with_test_writer()
            .set_default();

        let (recorder_address, mut recorder_context) =
            xtra::Context::<MessageRecordingActor>::new(None);
        let mut recorder = MessageRecordingActor::default();

        let commit_finality = Event::CommitFinality(OrderId::default());
        let refund_expired = Event::RefundTimelockExpired(OrderId::default());

        let mut monitor = Actor::for_test(
            recorder_address,
            [(
                (txid1(), script1()),
                vec![
                    (ScriptStatus::finality(), commit_finality.clone()),
                    (
                        ScriptStatus::with_confirmations(Cfd::CET_TIMELOCK),
                        refund_expired.clone(),
                    ),
                ],
            )],
        );
        monitor.client.include_tx(txid1(), 5);

        monitor.client.advance_to_height(10);
        recorder_context
            .handle_while(&mut recorder, monitor.sync())
            .await
            .unwrap();

        assert_eq!(recorder.events[0], commit_finality);

        monitor.client.advance_to_height(20);
        recorder_context
            .handle_while(&mut recorder, monitor.sync())
            .await
            .unwrap();

        assert_eq!(recorder.events[1], refund_expired);
    }

    #[tokio::test]
    async fn update_for_a_script_only_results_in_event_for_corresponding_transaction() {
        let _guard = tracing_subscriber::fmt()
            .with_env_filter("trace")
            .with_test_writer()
            .set_default();

        let (recorder_address, mut recorder_context) =
            xtra::Context::<MessageRecordingActor>::new(None);
        let mut recorder = MessageRecordingActor::default();

        let cet_finality = Event::CetFinality(OrderId::default());
        let refund_finality = Event::RefundFinality(OrderId::default());

        let mut monitor = Actor::for_test(
            recorder_address,
            [
                (
                    (txid1(), script1()),
                    vec![(ScriptStatus::finality(), cet_finality.clone())],
                ),
                (
                    (txid2(), script1()),
                    vec![(ScriptStatus::finality(), refund_finality.clone())],
                ),
            ],
        );
        monitor.client.include_tx(txid1(), 5);

        recorder_context
            .handle_while(&mut recorder, monitor.sync())
            .await
            .unwrap();

        assert!(recorder.events.contains(&cet_finality));
        assert!(!recorder.events.contains(&refund_finality));
    }

    impl<A> Actor<A, stub::Client>
    where
        A: xtra::Actor + xtra::Handler<Event>,
    {
        #[allow(clippy::type_complexity)]
        fn for_test<const N: usize>(
            address: xtra::Address<A>,
            subscriptions: [((Txid, Script), Vec<(ScriptStatus, Event)>); N],
        ) -> Self {
            Actor {
                cfds: HashMap::default(),
                cfd_actor_addr: address,
                client: stub::Client::default(),
                latest_block_height: BlockHeight(0),
                current_status: BTreeMap::default(),
                awaiting_status: HashMap::from_iter(subscriptions),
            }
        }
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

    #[derive(Default)]
    struct MessageRecordingActor {
        events: Vec<Event>,
    }

    impl xtra::Actor for MessageRecordingActor {}

    #[async_trait]
    impl xtra::Handler<Event> for MessageRecordingActor {
        async fn handle(&mut self, message: Event, _ctx: &mut xtra::Context<Self>) {
            self.events.push(message);
        }
    }

    mod stub {
        use super::*;
        use bdk::electrum_client::ScriptStatus;

        #[derive(Default)]
        pub struct Client {
            transactions: HashMap<Txid, i32>,
            block_height: usize,
        }

        impl Client {
            pub fn include_tx(&mut self, tx: Txid, height: i32) {
                self.transactions.insert(tx, height);
            }

            pub fn advance_to_height(&mut self, height: usize) {
                self.block_height = height;
            }
        }

        impl ElectrumApi for Client {
            fn block_headers_subscribe(&self) -> Result<HeaderNotification, Error> {
                Ok(HeaderNotification {
                    height: self.block_height,
                    header: block::BlockHeader {
                        version: 0,
                        prev_blockhash: Default::default(),
                        merkle_root: Default::default(),
                        time: 0,
                        bits: 0,
                        nonce: 0,
                    },
                })
            }

            fn batch_script_get_history<'s, I>(
                &self,
                _: I,
            ) -> Result<Vec<Vec<GetHistoryRes>>, Error>
            where
                I: IntoIterator<Item = &'s Script> + Clone,
            {
                Ok(self
                    .transactions
                    .iter()
                    .map(|(tx, included_at)| {
                        vec![GetHistoryRes {
                            height: *included_at,
                            tx_hash: *tx,
                            fee: None,
                        }]
                    })
                    .collect())
            }

            fn batch_call(&self, _batch: &Batch) -> Result<Vec<serde_json::Value>, Error> {
                unreachable!("This is a test.")
            }

            fn block_headers_subscribe_raw(&self) -> Result<RawHeaderNotification, Error> {
                unreachable!("This is a test.")
            }

            fn block_headers_pop_raw(&self) -> Result<Option<RawHeaderNotification>, Error> {
                unreachable!("This is a test.")
            }

            fn block_header_raw(&self, _height: usize) -> Result<Vec<u8>, Error> {
                unreachable!("This is a test.")
            }

            fn block_headers(&self, _: usize, _: usize) -> Result<GetHeadersRes, Error> {
                unreachable!("This is a test.")
            }

            fn estimate_fee(&self, _number: usize) -> Result<f64, Error> {
                unreachable!("This is a test.")
            }

            fn relay_fee(&self) -> Result<f64, Error> {
                unreachable!("This is a test.")
            }

            fn script_subscribe(&self, _script: &Script) -> Result<Option<ScriptStatus>, Error> {
                unreachable!("This is a test.")
            }

            fn script_unsubscribe(&self, _script: &Script) -> Result<bool, Error> {
                unreachable!("This is a test.")
            }

            fn script_pop(&self, _: &Script) -> Result<Option<ScriptStatus>, Error> {
                unreachable!("This is a test.")
            }

            fn script_get_balance(&self, _script: &Script) -> Result<GetBalanceRes, Error> {
                unreachable!("This is a test.")
            }

            fn batch_script_get_balance<'s, I>(&self, _: I) -> Result<Vec<GetBalanceRes>, Error>
            where
                I: IntoIterator<Item = &'s Script> + Clone,
            {
                unreachable!("This is a test.")
            }

            fn script_get_history(&self, _script: &Script) -> Result<Vec<GetHistoryRes>, Error> {
                unreachable!("This is a test.")
            }

            fn script_list_unspent(&self, _script: &Script) -> Result<Vec<ListUnspentRes>, Error> {
                unreachable!("This is a test.")
            }

            fn batch_script_list_unspent<'s, I>(
                &self,
                _: I,
            ) -> Result<Vec<Vec<ListUnspentRes>>, Error>
            where
                I: IntoIterator<Item = &'s Script> + Clone,
            {
                unreachable!("This is a test.")
            }

            fn transaction_get_raw(&self, _txid: &Txid) -> Result<Vec<u8>, Error> {
                unreachable!("This is a test.")
            }

            fn batch_transaction_get_raw<'t, I>(&self, _txids: I) -> Result<Vec<Vec<u8>>, Error>
            where
                I: IntoIterator<Item = &'t Txid> + Clone,
            {
                unreachable!("This is a test.")
            }

            fn batch_block_header_raw<I>(&self, _heights: I) -> Result<Vec<Vec<u8>>, Error>
            where
                I: IntoIterator<Item = u32> + Clone,
            {
                unreachable!("This is a test.")
            }

            fn batch_estimate_fee<I>(&self, _numbers: I) -> Result<Vec<f64>, Error>
            where
                I: IntoIterator<Item = usize> + Clone,
            {
                unreachable!("This is a test.")
            }

            fn transaction_broadcast_raw(&self, _raw_tx: &[u8]) -> Result<Txid, Error> {
                unreachable!("This is a test.")
            }

            fn transaction_get_merkle(&self, _: &Txid, _: usize) -> Result<GetMerkleRes, Error> {
                unreachable!("This is a test.")
            }

            fn server_features(&self) -> Result<ServerFeaturesRes, Error> {
                unreachable!("This is a test.")
            }

            fn ping(&self) -> Result<(), Error> {
                unreachable!("This is a test.")
            }
        }
    }
}
