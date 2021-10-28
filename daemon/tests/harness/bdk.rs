use bdk::bitcoin::util::psbt::{Global, PartiallySignedTransaction};
use bdk::bitcoin::{Transaction, Txid};
use std::collections::BTreeMap;

pub fn dummy_partially_signed_transaction() -> PartiallySignedTransaction {
    // very simple dummy psbt that does not contain anything
    // pulled in from github.com-1ecc6299db9ec823/bitcoin-0.27.1/src/util/psbt/mod.rs:238

    PartiallySignedTransaction {
        global: Global {
            unsigned_tx: Transaction {
                version: 2,
                lock_time: 0,
                input: vec![],
                output: vec![],
            },
            xpub: Default::default(),
            version: 0,
            proprietary: BTreeMap::new(),
            unknown: BTreeMap::new(),
        },
        inputs: vec![],
        outputs: vec![],
    }
}

pub fn dummy_tx_id() -> Txid {
    dummy_partially_signed_transaction().extract_tx().txid()
}
