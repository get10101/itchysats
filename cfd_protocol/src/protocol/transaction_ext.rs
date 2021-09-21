use anyhow::{Context, Result};
use bdk::bitcoin::{OutPoint, Script, Transaction};

pub trait TransactionExt {
    fn get_virtual_size(&self) -> f64;
    fn outpoint(&self, script_pubkey: &Script) -> Result<OutPoint>;
}

impl TransactionExt for Transaction {
    fn get_virtual_size(&self) -> f64 {
        self.get_weight() as f64 / 4.0
    }

    fn outpoint(&self, script_pubkey: &Script) -> Result<OutPoint> {
        let vout = self
            .output
            .iter()
            .position(|out| &out.script_pubkey == script_pubkey)
            .context("script pubkey not found in tx")?;

        Ok(OutPoint {
            txid: self.txid(),
            vout: vout as u32,
        })
    }
}
