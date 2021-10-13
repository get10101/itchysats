use crate::protocol::sighash_ext::SigHashExt;
use crate::protocol::transaction_ext::TransactionExt;
use crate::protocol::txin_ext::TxInExt;
use crate::protocol::{
    commit_descriptor, compute_adaptor_pk, lock_descriptor, Payout, DUMMY_2OF2_MULTISIG,
};
use anyhow::{Context, Result};
use bdk::bitcoin::util::bip143::SigHashCache;
use bdk::bitcoin::util::psbt::{Global, PartiallySignedTransaction};
use bdk::bitcoin::{
    Address, Amount, OutPoint, PublicKey, SigHash, SigHashType, Transaction, TxIn, TxOut,
};
use bdk::descriptor::Descriptor;
use bdk::miniscript::DescriptorTrait;
use itertools::Itertools;
use secp256k1_zkp::{self, schnorrsig, EcdsaAdaptorSignature, SecretKey, SECP256K1};
use std::collections::HashMap;
use std::iter::FromIterator;
use std::num::NonZeroU8;

/// In satoshi per vbyte.
const SATS_PER_VBYTE: f64 = 1.0;

pub(crate) fn lock_transaction(
    maker_psbt: PartiallySignedTransaction,
    taker_psbt: PartiallySignedTransaction,
    maker_pk: PublicKey,
    taker_pk: PublicKey,
    amount: Amount,
) -> PartiallySignedTransaction {
    let lock_descriptor = lock_descriptor(maker_pk, taker_pk);

    let maker_change = maker_psbt
        .global
        .unsigned_tx
        .output
        .into_iter()
        .filter(|out| {
            out.script_pubkey != DUMMY_2OF2_MULTISIG.parse().expect("To be a valid script")
        })
        .collect::<Vec<_>>();

    let taker_change = taker_psbt
        .global
        .unsigned_tx
        .output
        .into_iter()
        .filter(|out| {
            out.script_pubkey != DUMMY_2OF2_MULTISIG.parse().expect("To be a valid script")
        })
        .collect::<Vec<_>>();

    let lock_output = TxOut {
        value: amount.as_sat(),
        script_pubkey: lock_descriptor.script_pubkey(),
    };

    let input = vec![
        maker_psbt.global.unsigned_tx.input,
        taker_psbt.global.unsigned_tx.input,
    ]
    .concat();

    let output = std::iter::once(lock_output)
        .chain(maker_change)
        .chain(taker_change)
        .collect();

    let lock_tx = Transaction {
        version: 2,
        lock_time: 0,
        input,
        output,
    };

    PartiallySignedTransaction {
        global: Global::from_unsigned_tx(lock_tx).expect("to be unsigned"),
        inputs: vec![maker_psbt.inputs, taker_psbt.inputs].concat(),
        outputs: vec![maker_psbt.outputs, taker_psbt.outputs].concat(),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CommitTransaction {
    inner: Transaction,
    descriptor: Descriptor<PublicKey>,
    amount: Amount,
    sighash: SigHash,
    lock_descriptor: Descriptor<PublicKey>,
    fee: Fee,
}

impl CommitTransaction {
    /// Expected size of signed transaction in virtual bytes, plus a
    /// buffer to account for different signature lengths.
    const SIGNED_VBYTES: f64 = 148.5 + (3.0 * 2.0) / 4.0;

    pub(crate) fn new(
        lock_tx: &Transaction,
        (maker_pk, maker_rev_pk, maker_publish_pk): (PublicKey, PublicKey, PublicKey),
        (taker_pk, taker_rev_pk, taker_publish_pk): (PublicKey, PublicKey, PublicKey),
    ) -> Result<Self> {
        let lock_descriptor = lock_descriptor(maker_pk, taker_pk);
        let (lock_outpoint, lock_amount) = {
            let outpoint = lock_tx
                .outpoint(&lock_descriptor.script_pubkey())
                .context("lock script not found in lock tx")?;
            let amount = lock_tx.output[outpoint.vout as usize].value;

            (outpoint, amount)
        };

        let lock_input = TxIn {
            previous_output: lock_outpoint,
            ..Default::default()
        };

        let descriptor = commit_descriptor(
            (maker_pk, maker_rev_pk, maker_publish_pk),
            (taker_pk, taker_rev_pk, taker_publish_pk),
        );

        let output = TxOut {
            value: lock_amount,
            script_pubkey: descriptor.script_pubkey(),
        };

        let mut inner = Transaction {
            version: 2,
            lock_time: 0,
            input: vec![lock_input],
            output: vec![output],
        };
        let fee = Fee::new(Self::SIGNED_VBYTES);

        let commit_tx_amount = lock_amount - fee.as_u64();
        inner.output[0].value = commit_tx_amount;

        let sighash = SigHashCache::new(&inner).signature_hash(
            0,
            &lock_descriptor.script_code(),
            lock_amount,
            SigHashType::All,
        );

        Ok(Self {
            inner,
            descriptor,
            lock_descriptor,
            amount: Amount::from_sat(commit_tx_amount),
            sighash,
            fee,
        })
    }

    pub(crate) fn encsign(
        &self,
        sk: SecretKey,
        publish_them_pk: &PublicKey,
    ) -> EcdsaAdaptorSignature {
        EcdsaAdaptorSignature::encrypt(
            SECP256K1,
            &self.sighash.to_message(),
            &sk,
            &publish_them_pk.key,
        )
    }

    pub(crate) fn into_inner(self) -> Transaction {
        self.inner
    }

    fn outpoint(&self) -> OutPoint {
        self.inner
            .outpoint(&self.descriptor.script_pubkey())
            .expect("to find commit output in commit tx")
    }

    fn amount(&self) -> Amount {
        self.amount
    }

    fn descriptor(&self) -> Descriptor<PublicKey> {
        self.descriptor.clone()
    }

    fn fee(&self) -> u64 {
        self.fee.as_u64()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContractExecutionTransaction {
    inner: Transaction,
    index_nonce_pairs: Vec<(NonZeroU8, schnorrsig::PublicKey)>,
    sighash: SigHash,
    commit_descriptor: Descriptor<PublicKey>,
}

impl ContractExecutionTransaction {
    /// Expected size of signed transaction in virtual bytes, plus a
    /// buffer to account for different signature lengths.
    const SIGNED_VBYTES: f64 = 206.5 + (3.0 * 2.0) / 4.0;

    pub(crate) fn new(
        commit_tx: &CommitTransaction,
        payout: Payout,
        maker_address: &Address,
        taker_address: &Address,
        nonce_pks: &[schnorrsig::PublicKey],
        relative_timelock_in_blocks: u32,
    ) -> Result<Self> {
        let index_nonce_pairs: Vec<_> = payout
            .digits
            .to_indices()
            .into_iter()
            .zip(nonce_pks.iter().cloned())
            .collect();

        let commit_input = TxIn {
            previous_output: commit_tx.outpoint(),
            sequence: relative_timelock_in_blocks,
            ..Default::default()
        };

        let fee = Fee::new(Self::SIGNED_VBYTES).add(commit_tx.fee() as f64);
        let output = payout
            .with_updated_fee(
                Amount::from_sat(fee.as_u64()),
                maker_address.script_pubkey().dust_value(),
                taker_address.script_pubkey().dust_value(),
            )?
            .into_txouts(maker_address, taker_address);

        let tx = Transaction {
            version: 2,
            lock_time: 0,
            input: vec![commit_input],
            output,
        };

        let sighash = SigHashCache::new(&tx).signature_hash(
            0,
            &commit_tx.descriptor.script_code(),
            commit_tx.amount.as_sat(),
            SigHashType::All,
        );

        Ok(Self {
            inner: tx,
            index_nonce_pairs,
            sighash,
            commit_descriptor: commit_tx.descriptor(),
        })
    }

    pub(crate) fn encsign(
        &self,
        sk: SecretKey,
        oracle_pk: &schnorrsig::PublicKey,
    ) -> Result<EcdsaAdaptorSignature> {
        let adaptor_point = compute_adaptor_pk(oracle_pk, &self.index_nonce_pairs)?;

        Ok(EcdsaAdaptorSignature::encrypt(
            SECP256K1,
            &self.sighash.to_message(),
            &sk,
            &adaptor_point,
        ))
    }

    pub(crate) fn into_inner(self) -> Transaction {
        self.inner
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RefundTransaction {
    inner: Transaction,
    sighash: SigHash,
    commit_output_descriptor: Descriptor<PublicKey>,
}

impl RefundTransaction {
    /// Expected size of signed transaction in virtual bytes, plus a
    /// buffer to account for different signature lengths.
    const SIGNED_VBYTES: f64 = 206.5 + (3.0 * 2.0) / 4.0;

    pub(crate) fn new(
        commit_tx: &CommitTransaction,
        relative_locktime_in_blocks: u32,
        maker_address: &Address,
        taker_address: &Address,
        maker_amount: Amount,
        taker_amount: Amount,
    ) -> Self {
        let commit_input = TxIn {
            previous_output: commit_tx.outpoint(),
            sequence: relative_locktime_in_blocks,
            ..Default::default()
        };

        let maker_output = TxOut {
            value: maker_amount.as_sat(),
            script_pubkey: maker_address.script_pubkey(),
        };

        let taker_output = TxOut {
            value: taker_amount.as_sat(),
            script_pubkey: taker_address.script_pubkey(),
        };

        let mut tx = Transaction {
            version: 2,
            lock_time: 0,
            input: vec![commit_input],
            output: vec![maker_output, taker_output],
        };

        let mut fee = Self::SIGNED_VBYTES * SATS_PER_VBYTE;
        fee += commit_tx.fee() as f64;
        tx.output[0].value -= (fee / 2.0) as u64;
        tx.output[1].value -= (fee / 2.0) as u64;

        let commit_output_descriptor = commit_tx.descriptor();

        let sighash = SigHashCache::new(&tx).signature_hash(
            0,
            &commit_tx.descriptor().script_code(),
            commit_tx.amount().as_sat(),
            SigHashType::All,
        );

        Self {
            inner: tx,
            sighash,
            commit_output_descriptor,
        }
    }

    pub(crate) fn sighash(&self) -> SigHash {
        self.sighash
    }

    pub(crate) fn into_inner(self) -> Transaction {
        self.inner
    }
}

/// Build a transaction which closes the CFD contract.
///
/// This transaction spends directly from the lock transaction. Both
/// parties must agree on the split of coins between `maker_amount`
/// and `taker_amount`.
pub fn close_transaction(
    lock_descriptor: &Descriptor<PublicKey>,
    lock_outpoint: OutPoint,
    lock_amount: Amount,
    (maker_address, maker_amount): (&Address, Amount),
    (taker_address, taker_amount): (&Address, Amount),
) -> Result<(Transaction, secp256k1_zkp::Message)> {
    /// Expected size of signed transaction in virtual bytes, plus a
    /// buffer to account for different signature lengths.
    const SIGNED_VBYTES: f64 = 167.5 + (3.0 * 2.0) / 4.0;

    let lock_input = TxIn {
        previous_output: lock_outpoint,
        ..Default::default()
    };

    // TODO: The fee could take into account the network state in this
    // case, since this transaction is to be broadcast immediately
    // after building and signing it
    let (maker_fee, taker_fee) = Fee::new(SIGNED_VBYTES).split();

    let maker_output = TxOut {
        value: maker_amount.as_sat() - maker_fee,
        script_pubkey: maker_address.script_pubkey(),
    };
    let taker_output = TxOut {
        value: taker_amount.as_sat() - taker_fee,
        script_pubkey: taker_address.script_pubkey(),
    };

    let tx = Transaction {
        version: 2,
        lock_time: 0,
        input: vec![lock_input],
        output: vec![maker_output, taker_output],
    };

    let sighash = SigHashCache::new(&tx)
        .signature_hash(
            0,
            &lock_descriptor.script_code(),
            lock_amount.as_sat(),
            SigHashType::All,
        )
        .to_message();

    Ok((tx, sighash))
}

pub fn punish_transaction(
    commit_descriptor: &Descriptor<PublicKey>,
    address: &Address,
    encsig: EcdsaAdaptorSignature,
    sk: SecretKey,
    revocation_them_sk: SecretKey,
    pub_them_pk: PublicKey,
    revoked_commit_tx: &Transaction,
) -> Result<Transaction> {
    /// Expected size of signed transaction in virtual bytes, plus a
    /// buffer to account for different signature lengths.
    const SIGNED_VBYTES: f64 = 219.5 + (3.0 * 3.0) / 4.0;

    let input = revoked_commit_tx
        .input
        .clone()
        .into_iter()
        .exactly_one()
        .context("commit transaction inputs != 1")?;

    let publish_them_sk = input
        .find_map_signature(|sig| encsig.recover(SECP256K1, &sig, &pub_them_pk.key).ok())
        .context("could not recover publish sk from commit tx")?;

    let commit_outpoint = revoked_commit_tx
        .outpoint(&commit_descriptor.script_pubkey())
        .expect("to find commit output in commit tx");
    let commit_amount = revoked_commit_tx.output[commit_outpoint.vout as usize].value;

    let mut punish_tx = {
        let output = TxOut {
            value: commit_amount,
            script_pubkey: address.script_pubkey(),
        };
        let mut tx = Transaction {
            version: 2,
            lock_time: 0,
            input: vec![TxIn {
                previous_output: commit_outpoint,
                ..Default::default()
            }],
            output: vec![output],
        };

        let fee = SIGNED_VBYTES * SATS_PER_VBYTE;
        tx.output[0].value = commit_amount - fee as u64;

        tx
    };

    let sighash = SigHashCache::new(&punish_tx).signature_hash(
        0,
        &commit_descriptor.script_code(),
        commit_amount,
        SigHashType::All,
    );

    let satisfier = {
        let pk = {
            let key = secp256k1_zkp::PublicKey::from_secret_key(SECP256K1, &sk);
            PublicKey {
                compressed: true,
                key,
            }
        };
        let pk_hash = pk.pubkey_hash().as_hash();
        let sig_sk = SECP256K1.sign(&sighash.to_message(), &sk);

        let pub_them_pk_hash = pub_them_pk.pubkey_hash().as_hash();
        let sig_pub_them = SECP256K1.sign(&sighash.to_message(), &publish_them_sk);

        let rev_them_pk = {
            let key = secp256k1_zkp::PublicKey::from_secret_key(SECP256K1, &revocation_them_sk);
            PublicKey {
                compressed: true,
                key,
            }
        };
        let rev_them_pk_hash = rev_them_pk.pubkey_hash().as_hash();
        let sig_rev_them = SECP256K1.sign(&sighash.to_message(), &revocation_them_sk);

        let sighash_all = SigHashType::All;
        HashMap::from_iter(vec![
            (pk_hash, (pk, (sig_sk, sighash_all))),
            (pub_them_pk_hash, (pub_them_pk, (sig_pub_them, sighash_all))),
            (rev_them_pk_hash, (rev_them_pk, (sig_rev_them, sighash_all))),
        ])
    };

    commit_descriptor.satisfy(&mut punish_tx.input[0], satisfier)?;

    Ok(punish_tx)
}

#[derive(Clone, Debug)]
struct Fee {
    fee: f64,
}

impl Fee {
    fn new(signed_vbytes: f64) -> Self {
        let fee = signed_vbytes * SATS_PER_VBYTE;
        Self { fee }
    }

    #[must_use]
    fn add(self, number: f64) -> Fee {
        Fee {
            fee: self.fee + number,
        }
    }

    fn as_u64(&self) -> u64 {
        // Ceil to prevent going lower than the min relay fee
        self.fee.ceil() as u64
    }

    fn split(&self) -> (u64, u64) {
        let half = self.as_u64() / 2;
        (half as u64, self.as_u64() - half)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_fee_always_above_min_relay_fee(signed_vbytes in 1.0f64..100_000_000.0f64) {
            let fee = Fee::new(signed_vbytes);
            let (maker_fee, taker_fee) = fee.split();

            prop_assert!(signed_vbytes <= fee.as_u64() as f64);
            prop_assert!(signed_vbytes <= (maker_fee + taker_fee) as f64);
        }
    }

    // A bunch of tests illustrating how fees are split

    #[test]
    fn test_splitting_fee_1_0() {
        const SIGNED_VBYTES_TEST: f64 = 1.0;

        let fee = Fee::new(SIGNED_VBYTES_TEST);
        let (maker_fee, taker_fee) = fee.split();

        assert_eq!(fee.as_u64(), 1);
        assert_eq!(maker_fee, 0);
        assert_eq!(taker_fee, 1);
        assert!((maker_fee + taker_fee) as f64 >= SIGNED_VBYTES_TEST);
    }

    #[test]
    fn test_splitting_fee_2_0() {
        const SIGNED_VBYTES_TEST: f64 = 2.0;

        let fee = Fee::new(SIGNED_VBYTES_TEST);
        let (maker_fee, taker_fee) = fee.split();

        assert_eq!(fee.as_u64(), 2);
        assert_eq!(maker_fee, 1);
        assert_eq!(taker_fee, 1);
        assert!((maker_fee + taker_fee) as f64 >= SIGNED_VBYTES_TEST);
    }

    #[test]
    fn test_splitting_fee_2_1() {
        const SIGNED_VBYTES_TEST: f64 = 2.1;

        let fee = Fee::new(SIGNED_VBYTES_TEST);
        let (maker_fee, taker_fee) = fee.split();

        assert_eq!(fee.as_u64(), 3);
        assert_eq!(maker_fee, 1);
        assert_eq!(taker_fee, 2);
        assert!((maker_fee + taker_fee) as f64 >= SIGNED_VBYTES_TEST);
    }

    #[test]
    fn test_splitting_fee_2_6() {
        const SIGNED_VBYTES_TEST: f64 = 2.6;

        let fee = Fee::new(SIGNED_VBYTES_TEST);
        let (maker_fee, taker_fee) = fee.split();

        assert_eq!(fee.as_u64(), 3);
        assert_eq!(maker_fee, 1);
        assert_eq!(taker_fee, 2);
        assert!((maker_fee + taker_fee) as f64 >= SIGNED_VBYTES_TEST);
    }
}
