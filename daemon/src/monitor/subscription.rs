#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use bdk::bitcoin::{Script, Txid};
use bdk::electrum_client::{ElectrumApi, GetHistoryRes, HeaderNotification};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::convert::{TryFrom, TryInto};
use std::fmt;
use std::ops::Add;
use std::sync::Arc;
use tokio::sync::{watch, Mutex};
use tokio::time::{Duration, Instant};

pub struct Monitor {
    client: Arc<Mutex<Client>>,
    finality_confirmations: u32,
}

impl Monitor {
    pub fn new(electrum_rpc_url: &str, finality_confirmations: u32) -> Result<Self> {
        let client = bdk::electrum_client::Client::new(electrum_rpc_url)
            .context("Failed to initialize Electrum RPC client")?;

        let client = Client::new(client, Duration::from_secs(10))?;

        let monitor = Monitor {
            client: Arc::new(Mutex::new(client)),
            finality_confirmations,
        };

        Ok(monitor)
    }

    pub async fn subscribe_to(&self, tx: impl Watchable + Send + 'static) -> Subscription {
        let txid = tx.id();
        let script = tx.script();

        let sub = self
            .client
            .lock()
            .await
            .subscriptions
            .entry((txid, script.clone()))
            .or_insert_with(|| {
                let (sender, receiver) = watch::channel(ScriptStatus::Unseen);
                let client = self.client.clone();

                tokio::spawn(async move {
                    let mut last_status = None;

                    // TODO: We need feedback in the monitoring actor about failures in here
                    loop {
                        tokio::time::sleep(Duration::from_secs(5)).await;

                        let new_status = match client.lock().await.status_of_script(&tx) {
                            Ok(new_status) => new_status,
                            Err(error) => {
                                tracing::warn!(%txid, "Failed to get status of script: {:#}", error);
                                return;
                            }
                        };

                        last_status = Some(print_status_change(txid, last_status, new_status));

                        let all_receivers_gone = sender.send(new_status).is_err();

                        if all_receivers_gone {
                            tracing::debug!(%txid, "All receivers gone, removing subscription");
                            client.lock().await.subscriptions.remove(&(txid, script));
                            return;
                        }
                    }
                });

                Subscription {
                    receiver,
                    finality_confirmations: self.finality_confirmations,
                    txid,
                }
            })
            .clone();

        sub
    }
}

/// Represents a subscription to the status of a given transaction.
#[derive(Debug, Clone)]
pub struct Subscription {
    receiver: watch::Receiver<ScriptStatus>,
    finality_confirmations: u32,
    txid: Txid,
}

impl Subscription {
    pub async fn wait_until_final(&self) -> Result<()> {
        let conf_target = self.finality_confirmations;
        let txid = self.txid;

        tracing::info!(%txid, required_confirmation=%conf_target, "Waiting for Bitcoin transaction finality");

        let mut seen_confirmations = 0;

        self.wait_until(|status| match status {
            ScriptStatus::Confirmed(inner) => {
                let confirmations = inner.confirmations();

                if confirmations > seen_confirmations {
                    tracing::info!(%txid,
                        seen_confirmations = %confirmations,
                        needed_confirmations = %conf_target,
                        "Waiting for Bitcoin transaction finality");
                    seen_confirmations = confirmations;
                }

                inner.meets_target(conf_target)
            }
            _ => false,
        })
        .await
    }

    pub async fn wait_until_seen(&self) -> Result<()> {
        self.wait_until(ScriptStatus::has_been_seen).await
    }

    pub async fn wait_until_confirmed_with<T>(&self, target: T) -> Result<()>
    where
        u32: PartialOrd<T>,
        T: Copy,
    {
        self.wait_until(|status| status.is_confirmed_with(target))
            .await
    }

    async fn wait_until(&self, mut predicate: impl FnMut(&ScriptStatus) -> bool) -> Result<()> {
        let mut receiver = self.receiver.clone();

        while !predicate(&receiver.borrow()) {
            receiver
                .changed()
                .await
                .context("Failed while waiting for next status update")?;
        }

        Ok(())
    }
}

/// Defines a watchable transaction.
///
/// For a transaction to be watchable, we need to know two things: Its
/// transaction ID and the specific output script that is going to change.
/// A transaction can obviously have multiple outputs but our protocol purposes,
/// we are usually interested in a specific one.
pub trait Watchable {
    fn id(&self) -> Txid;
    fn script(&self) -> Script;
}

impl Watchable for (Txid, Script) {
    fn id(&self) -> Txid {
        self.0
    }

    fn script(&self) -> Script {
        self.1.clone()
    }
}

fn print_status_change(txid: Txid, old: Option<ScriptStatus>, new: ScriptStatus) -> ScriptStatus {
    match (old, new) {
        (None, new_status) => {
            tracing::debug!(%txid, status = %new_status, "Found relevant Bitcoin transaction");
        }
        (Some(old_status), new_status) if old_status != new_status => {
            tracing::debug!(%txid, %new_status, %old_status, "Bitcoin transaction status changed");
        }
        _ => {}
    }

    new
}

pub struct Client {
    electrum: bdk::electrum_client::Client,
    latest_block_height: BlockHeight,
    last_sync: Instant,
    sync_interval: Duration,
    script_history: BTreeMap<Script, Vec<GetHistoryRes>>,
    subscriptions: HashMap<(Txid, Script), Subscription>,
}

impl Client {
    fn new(electrum: bdk::electrum_client::Client, interval: Duration) -> Result<Self> {
        // Initially fetch the latest block for storing the height.
        // We do not act on this subscription after this call.
        let latest_block = electrum
            .block_headers_subscribe()
            .context("Failed to subscribe to header notifications")?;

        Ok(Self {
            electrum,
            latest_block_height: BlockHeight::try_from(latest_block)?,
            last_sync: Instant::now(),
            sync_interval: interval,
            script_history: Default::default(),
            subscriptions: Default::default(),
        })
    }

    fn update_state(&mut self) -> Result<()> {
        let now = Instant::now();
        if now < self.last_sync + self.sync_interval {
            return Ok(());
        }

        self.last_sync = now;
        self.update_latest_block()?;
        self.update_script_histories()?;

        Ok(())
    }

    fn status_of_script<T>(&mut self, tx: &T) -> Result<ScriptStatus>
    where
        T: Watchable,
    {
        let txid = tx.id();
        let script = tx.script();

        if !self.script_history.contains_key(&script) {
            self.script_history.insert(script.clone(), vec![]);
        }

        self.update_state()?;

        let history = self.script_history.entry(script).or_default();

        let history_of_tx = history
            .iter()
            .filter(|entry| entry.tx_hash == txid)
            .collect::<Vec<_>>();

        match history_of_tx.as_slice() {
            [] => Ok(ScriptStatus::Unseen),
            [remaining @ .., last] => {
                if !remaining.is_empty() {
                    tracing::warn!("Found more than a single history entry for script. This is highly unexpected and those history entries will be ignored")
                }

                if last.height <= 0 {
                    Ok(ScriptStatus::InMempool)
                } else {
                    Ok(ScriptStatus::Confirmed(
                        Confirmed::from_inclusion_and_latest_block(
                            u32::try_from(last.height)?,
                            u32::from(self.latest_block_height),
                        ),
                    ))
                }
            }
        }
    }

    fn update_latest_block(&mut self) -> Result<()> {
        // Fetch the latest block for storing the height.
        // We do not act on this subscription after this call, as we cannot rely on
        // subscription push notifications because eventually the Electrum server will
        // close the connection and subscriptions are not automatically renewed
        // upon renewing the connection.
        let latest_block = self
            .electrum
            .block_headers_subscribe()
            .context("Failed to subscribe to header notifications")?;
        let latest_block_height = BlockHeight::try_from(latest_block)?;

        if latest_block_height > self.latest_block_height {
            tracing::debug!(
                block_height = u32::from(latest_block_height),
                "Got notification for new block"
            );
            self.latest_block_height = latest_block_height;
        }

        Ok(())
    }

    fn update_script_histories(&mut self) -> Result<()> {
        let histories = self
            .electrum
            .batch_script_get_history(self.script_history.keys())
            .context("Failed to get script histories")?;

        if histories.len() != self.script_history.len() {
            bail!(
                "Expected {} history entries, received {}",
                self.script_history.len(),
                histories.len()
            );
        }

        let scripts = self.script_history.keys().cloned();
        let histories = histories.into_iter();

        self.script_history = scripts.zip(histories).collect::<BTreeMap<_, _>>();

        Ok(())
    }
}

/// Represent a block height, or block number, expressed in absolute block
/// count. E.g. The transaction was included in block #655123, 655123 block
/// after the genesis block.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlockHeight(u32);

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExpiredTimelocks {
    None,
    Cancel,
    Punish,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct Confirmed {
    /// The depth of this transaction within the blockchain.
    ///
    /// Will be zero if the transaction is included in the latest block.
    depth: u32,
}

impl Confirmed {
    pub fn new(depth: u32) -> Self {
        Self { depth }
    }

    /// Compute the depth of a transaction based on its inclusion height and the
    /// latest known block.
    ///
    /// Our information about the latest block might be outdated. To avoid an
    /// overflow, we make sure the depth is 0 in case the inclusion height
    /// exceeds our latest known block,
    pub fn from_inclusion_and_latest_block(inclusion_height: u32, latest_block: u32) -> Self {
        let depth = latest_block.saturating_sub(inclusion_height);

        Self { depth }
    }

    pub fn confirmations(&self) -> u32 {
        self.depth + 1
    }

    pub fn meets_target<T>(&self, target: T) -> bool
    where
        u32: PartialOrd<T>,
    {
        self.confirmations() >= target
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum ScriptStatus {
    Unseen,
    InMempool,
    Confirmed(Confirmed),
}

impl ScriptStatus {
    /// Check if the script has any confirmations.
    pub fn is_confirmed(&self) -> bool {
        matches!(self, ScriptStatus::Confirmed(_))
    }

    /// Check if the script has met the given confirmation target.
    pub fn is_confirmed_with<T>(&self, target: T) -> bool
    where
        u32: PartialOrd<T>,
    {
        match self {
            ScriptStatus::Confirmed(inner) => inner.meets_target(target),
            _ => false,
        }
    }

    pub fn has_been_seen(&self) -> bool {
        matches!(self, ScriptStatus::InMempool | ScriptStatus::Confirmed(_))
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
