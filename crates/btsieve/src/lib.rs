use bitcoin::Script;
use bitcoin::Txid;
use std::collections::hash_map::Entry;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt;

pub struct State<E> {
    latest_block_height: BlockHeight,
    current_status: BTreeMap<(Txid, Script), ScriptStatus>,
    awaiting_status: HashMap<(Txid, Script), Vec<(ScriptStatus, E)>>,
}

impl<E> State<E> {
    pub fn new(latest_block_height: BlockHeight) -> Self {
        State {
            latest_block_height,
            current_status: BTreeMap::default(),
            awaiting_status: HashMap::default(),
        }
    }

    /// Returns the number of transactions/scripts that we are currently monitoring.
    pub fn num_monitoring(&self) -> usize {
        self.awaiting_status.len()
    }

    /// Returns all scripts that we are currently monitoring.
    pub fn monitoring_scripts(&self) -> impl Iterator<Item = &Script> + Clone {
        self.awaiting_status.keys().map(|(_, script)| script)
    }

    pub fn monitor(&mut self, txid: Txid, script: Script, script_status: ScriptStatus, event: E) {
        self.awaiting_status
            .entry((txid, script))
            .or_default()
            .push((script_status, event));
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TxStatus {
    /// Confirmation height of the transaction.
    ///
    /// 0 if unconfirmed.
    /// -1 if unconfirmed while some of its inputs are unconfirmed too.
    pub height: i32,
    pub tx_hash: Txid,
}

impl<E> State<E>
where
    E: fmt::Debug,
{
    pub fn update(
        &mut self,
        latest_block_height: BlockHeight,
        status_list: Vec<TxStatus>,
    ) -> Vec<E> {
        let txid_to_script = self
            .awaiting_status
            .keys()
            .cloned()
            .collect::<HashMap<_, _>>();

        let mut status_map = HashMap::new();
        for status in status_list {
            let txid = status.tx_hash;
            let script = match txid_to_script.get(&txid) {
                None => {
                    tracing::trace!("Could not find script in own state for txid {txid}, ignoring");
                    continue;
                }
                Some(script) => script,
            };
            status_map.insert((txid, script.clone()), status);
        }

        if latest_block_height > self.latest_block_height {
            tracing::trace!(
                block_height = %latest_block_height,
                "Got notification for new block"
            );
            self.latest_block_height = latest_block_height;
        }

        // 1. Decide new status based on script history
        let new_status = self
            .awaiting_status
            .iter()
            .map(|(key, _old_status)| {
                let new_script_status = match status_map.get(key) {
                    None => ScriptStatus::Unseen,
                    Some(status) => {
                        if status.height <= 0 {
                            ScriptStatus::InMempool
                        } else {
                            ScriptStatus::Confirmed(Confirmed::from_inclusion_and_latest_block(
                                u32::try_from(status.height)
                                    .expect("we checked that height is > 0"),
                                self.latest_block_height,
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
                        tracing::debug!(%txid, target = %target_status, current = %status, "Bitcoin transaction reached monitoring target");
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

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd)]
pub struct Confirmed {
    /// The depth of this transaction within the blockchain.
    ///
    /// Will be zero if the transaction is included in the latest block.
    depth: u32,
}

impl Confirmed {
    pub fn with_confirmations(blocks: u32) -> Self {
        Self { depth: blocks - 1 }
    }

    /// Compute the depth of a transaction based on its inclusion height and the
    /// latest known block.
    ///
    /// Our information about the latest block might be outdated. To avoid an
    /// overflow, we make sure the depth is 0 in case the inclusion height
    /// exceeds our latest known block,
    fn from_inclusion_and_latest_block(inclusion_height: u32, latest_block: BlockHeight) -> Self {
        let depth = latest_block.0.saturating_sub(inclusion_height);

        Self { depth }
    }

    fn confirmations(&self) -> u32 {
        self.depth + 1
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd)]
pub enum ScriptStatus {
    Unseen,
    InMempool,
    Confirmed(Confirmed),
}

impl ScriptStatus {
    pub fn with_confirmations(confirmations: u32) -> Self {
        Self::Confirmed(Confirmed::with_confirmations(confirmations))
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
pub struct BlockHeight(u32);

impl From<usize> for BlockHeight {
    fn from(height: usize) -> Self {
        let height = u32::try_from(height)
            .expect("bitcoin block count exceeds u32::MAX in > 80_000 years; qed");

        Self(height)
    }
}

impl fmt::Display for BlockHeight {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Event {
        FooFinality,
        BarFinality,
        BazTimelockExpired,
    }

    #[test]
    fn can_handle_multiple_subscriptions_on_the_same_transaction() {
        let _guard = tracing_subscriber::fmt()
            .with_env_filter("trace")
            .with_test_writer()
            .set_default();

        let foo_finality = Event::FooFinality;
        let baz_expired = Event::BazTimelockExpired;

        let mut state = State::new(BlockHeight(0));
        state.awaiting_status = HashMap::from_iter([(
            (txid1(), script1()),
            vec![
                (ScriptStatus::with_confirmations(1), foo_finality),
                (ScriptStatus::with_confirmations(12), baz_expired),
            ],
        )]);

        let ready_events = state.update(
            BlockHeight(10),
            vec![TxStatus {
                height: 5,
                tx_hash: txid1(),
            }],
        );

        assert_eq!(ready_events, vec![foo_finality]);

        let ready_events = state.update(
            BlockHeight(20),
            vec![TxStatus {
                height: 5,
                tx_hash: txid1(),
            }],
        );

        assert_eq!(ready_events, vec![baz_expired]);
    }

    #[test]
    fn update_for_a_script_only_results_in_event_for_corresponding_transaction() {
        let _guard = tracing_subscriber::fmt()
            .with_env_filter("trace")
            .with_test_writer()
            .set_default();

        let bar_finality = Event::BarFinality;
        let foo_finality = Event::FooFinality;

        let mut state = State::new(BlockHeight(0));
        state.awaiting_status = HashMap::from_iter([
            (
                (txid1(), script1()),
                vec![(ScriptStatus::with_confirmations(1), bar_finality)],
            ),
            (
                (txid2(), script1()),
                vec![(ScriptStatus::with_confirmations(1), foo_finality)],
            ),
        ]);

        let ready_events = state.update(
            BlockHeight(0),
            vec![TxStatus {
                height: 5,
                tx_hash: txid1(),
            }],
        );

        assert_eq!(ready_events, vec![bar_finality]);
    }

    #[test]
    fn stop_monitoring_after_target_reached() {
        let _guard = tracing_subscriber::fmt()
            .with_env_filter("trace")
            .with_test_writer()
            .set_default();

        let foo_finality = Event::FooFinality;

        let mut state = State::new(BlockHeight(0));
        state.awaiting_status = HashMap::from_iter([(
            (txid1(), script1()),
            vec![(ScriptStatus::with_confirmations(1), foo_finality)],
        )]);

        let ready_events = state.update(
            BlockHeight(0),
            vec![TxStatus {
                height: 5,
                tx_hash: txid1(),
            }],
        );

        assert_eq!(ready_events, vec![foo_finality]);
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
