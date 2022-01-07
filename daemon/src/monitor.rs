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
use crate::Tasks;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::PublicKey;
use bdk::bitcoin::Script;
use bdk::bitcoin::Txid;
use bdk::descriptor::Descriptor;
use bdk::electrum_client::ElectrumApi;
use bdk::electrum_client::GetHistoryRes;
use bdk::electrum_client::HeaderNotification;
use bdk::miniscript::DescriptorTrait;
use sqlx::SqlitePool;
use std::collections::hash_map::Entry;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::convert::TryInto;
use std::fmt;
use std::marker::Send;
use std::ops::Add;
use std::ops::RangeInclusive;
use std::time::Duration;
use xtra::prelude::StrongMessageChannel;
use xtra_productivity::xtra_productivity;

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

pub struct Sync;

// TODO: Send messages to the projection actor upon finality events so we send out updates.
//  -> Might as well just send out all events independent of sending to the cfd actor.
pub struct Actor<C = bdk::electrum_client::Client> {
    cfds: HashMap<OrderId, MonitorParams>,
    event_channel: Box<dyn StrongMessageChannel<Event>>,
    client: C,
    latest_block_height: BlockHeight,
    current_status: BTreeMap<(Txid, Script), ScriptStatus>,
    awaiting_status: HashMap<(Txid, Script), Vec<(ScriptStatus, Event)>>,
    tasks: Tasks,
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
                params: Some(MonitorParams::new(dlc)),
                monitor_lock_finality: true,
                monitor_commit_finality: true,
                monitor_cet_timelock: true,
                monitor_refund_timelock: true,
                monitor_refund_finality: true,
                monitor_revoked_commit_transactions: false,
                monitor_collaborative_settlement_finality: None,
            },
            RolloverCompleted { dlc } => {
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
                }
            }
            CollaborativeSettlementCompleted {
                spend_tx, script, ..
            } => {
                Self {
                    monitor_lock_finality: false, // Lock is already final if we collab settle.
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
                ..self
            },
            CommitConfirmed => Self {
                monitor_commit_finality: false,
                ..self
            },
            // final states, don't monitor anything
            CetConfirmed | RefundConfirmed | CollaborativeSettlementConfirmed => Self::default(),
            CetTimelockConfirmedPriorOracleAttestation
            | CetTimelockConfirmedPostOracleAttestation { .. } => Self {
                monitor_cet_timelock: false,
                ..self
            },
            RefundTimelockConfirmed { .. } => Self {
                monitor_refund_timelock: false,
                ..self
            },
            RolloverStarted { .. }
            | RolloverAccepted
            | RolloverFailed
            | ManualCommit { .. }
            | OracleAttestedPostCetTimelock { .. }
            | OracleAttestedPriorCetTimelock { .. }
            | CollaborativeSettlementRejected { .. }
            | CollaborativeSettlementFailed { .. }
            | CollaborativeSettlementStarted { .. }
            | CollaborativeSettlementProposalAccepted => self,
            RevokeConfirmed => todo!("Deal with revoked"),
        }
    }
}

impl Actor<bdk::electrum_client::Client> {
    pub async fn new(
        db: SqlitePool,
        electrum_rpc_url: String,
        event_channel: Box<dyn StrongMessageChannel<Event>>,
    ) -> Result<Self> {
        let client = bdk::electrum_client::Client::new(&electrum_rpc_url)
            .context("Failed to initialize Electrum RPC client")?;

        // Initially fetch the latest block for storing the height.
        // We do not act on this subscription after this call.
        let latest_block = client
            .block_headers_subscribe()
            .context("Failed to subscribe to header notifications")?;

        let mut actor = Self {
            cfds: HashMap::new(),
            event_channel,
            client,
            latest_block_height: BlockHeight::try_from(latest_block)?,
            current_status: BTreeMap::default(),
            awaiting_status: HashMap::default(),
            tasks: Tasks::default(),
        };

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
            } = events.into_iter().fold(Cfd::default(), Cfd::apply);

            let params = match params {
                None => continue,
                Some(params) => params,
            };

            actor.cfds.insert(id, params.clone());

            if monitor_lock_finality {
                actor.monitor_lock_finality(&params, id);
            }

            if monitor_commit_finality {
                actor.monitor_commit_finality(&params, id)
            }

            if monitor_cet_timelock {
                actor.monitor_commit_cet_timelock(&params, id);
            }

            if monitor_refund_timelock {
                actor.monitor_commit_refund_timelock(&params, id);
            }

            if monitor_refund_finality {
                actor.monitor_refund_finality(&params, id);
            }

            if monitor_revoked_commit_transactions {
                actor.monitor_revoked_commit_transactions(&params, id);
            }

            if let Some(params) = monitor_collaborative_settlement_finality {
                actor.monitor_close_finality(params, id);
            }
        }

        Ok(actor)
    }
}

impl<C> Actor<C>
where
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
            format!(
                "No CET for oracle event {} and cfd with order id {}",
                attestation_id, order_id
            )
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
                        tracing::trace!(
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

    async fn handle_oracle_attestation(&mut self, attestation: oracle::Attestation) {
        for (order_id, MonitorParams { cets, .. }) in self
            .cfds
            .clone()
            .into_iter()
            .filter(|(_, params)| params.event_id == attestation.id)
        {
            try_continue!(self.monitor_cet_finality(cets, attestation.clone(), order_id))
        }
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
                        self.event_channel.send(event).await?;
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

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    LockFinality(OrderId),
    CommitFinality(OrderId),
    CloseFinality(OrderId),
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
            Event::CloseFinality(order_id) => order_id,
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
    pub fn new(dlc: Dlc) -> Self {
        let script_pubkey = dlc.maker_address.script_pubkey();
        MonitorParams {
            lock: (dlc.lock.0.txid(), dlc.lock.1),
            commit: (dlc.commit.0.txid(), dlc.commit.2),
            cets: map_cets(dlc.cets),
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

impl From<model::cfd::Cet> for Cet {
    fn from(cet: model::cfd::Cet) -> Self {
        Cet {
            txid: cet.tx.txid(),
            script: cet.tx.output[0].script_pubkey.clone(),
            range: cet.range.clone(),
        }
    }
}

fn map_cets(
    cets: HashMap<BitMexPriceEventId, Vec<model::cfd::Cet>>,
) -> HashMap<BitMexPriceEventId, Vec<Cet>> {
    cets.into_iter()
        .map(|(event_id, cets)| {
            (
                event_id,
                cets.into_iter().map(Cet::from).collect::<Vec<_>>(),
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

#[async_trait]
impl<C> xtra::Actor for Actor<C>
where
    C: Send + 'static,
    Self: xtra::Handler<Sync>,
{
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let fut = ctx
            .notify_interval(Duration::from_secs(20), || Sync)
            .expect("we just started");

        self.tasks.add(fut);
    }
}

#[xtra_productivity]
impl<C> Actor<C>
where
    C: bdk::electrum_client::ElectrumApi + Send + 'static,
{
    async fn handle_start_monitoring(
        &mut self,
        msg: StartMonitoring,
        _ctx: &mut xtra::Context<Self>,
    ) {
        let StartMonitoring { id, params } = msg;

        self.monitor_all(&params, id);
        self.cfds.insert(id, params);
    }

    fn handle_collaborative_settlement(
        &mut self,
        collaborative_settlement: CollaborativeSettlement,
    ) {
        self.monitor_close_finality(
            collaborative_settlement.tx,
            collaborative_settlement.order_id,
        );
    }
}

#[async_trait]
impl<C> xtra::Handler<Sync> for Actor<C>
where
    C: bdk::electrum_client::ElectrumApi + Send + 'static,
{
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
    use bdk::bitcoin::blockdata::block;
    use bdk::electrum_client::Batch;
    use bdk::electrum_client::Error;
    use bdk::electrum_client::GetBalanceRes;
    use bdk::electrum_client::GetHeadersRes;
    use bdk::electrum_client::GetMerkleRes;
    use bdk::electrum_client::ListUnspentRes;
    use bdk::electrum_client::RawHeaderNotification;
    use bdk::electrum_client::ServerFeaturesRes;
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
            Box::new(recorder_address),
            [(
                (txid1(), script1()),
                vec![
                    (ScriptStatus::finality(), commit_finality.clone()),
                    (
                        ScriptStatus::with_confirmations(CET_TIMELOCK),
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
            Box::new(recorder_address),
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

    #[tokio::test]
    async fn stop_monitoring_after_target_reached() {
        let _guard = tracing_subscriber::fmt()
            .with_env_filter("trace")
            .with_test_writer()
            .set_default();

        let (recorder_address, mut recorder_context) =
            xtra::Context::<MessageRecordingActor>::new(None);
        let mut recorder = MessageRecordingActor::default();

        let cet_finality = Event::CetFinality(OrderId::default());

        let mut monitor = Actor::for_test(
            Box::new(recorder_address),
            [(
                (txid1(), script1()),
                vec![(ScriptStatus::finality(), cet_finality.clone())],
            )],
        );
        monitor.client.include_tx(txid1(), 5);

        recorder_context
            .handle_while(&mut recorder, monitor.sync())
            .await
            .unwrap();

        assert!(recorder.events.contains(&cet_finality));
        assert!(monitor.awaiting_status.is_empty());
    }

    impl Actor<stub::Client> {
        #[allow(clippy::type_complexity)]
        fn for_test<const N: usize>(
            event_channel: Box<dyn StrongMessageChannel<Event>>,
            subscriptions: [((Txid, Script), Vec<(ScriptStatus, Event)>); N],
        ) -> Self {
            Actor {
                cfds: HashMap::default(),
                event_channel,
                client: stub::Client::default(),
                latest_block_height: BlockHeight(0),
                current_status: BTreeMap::default(),
                awaiting_status: HashMap::from_iter(subscriptions),
                tasks: Tasks::default(),
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
