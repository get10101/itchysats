use crate::protocol::sighash_ext::SigHashExt;
use crate::protocol::transaction_ext::TransactionExt;
use crate::protocol::{
    commit_descriptor, compute_adaptor_point, lock_descriptor, Payout, DUMMY_2OF2_MULTISIG,
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
use secp256k1_zkp::{self, schnorrsig, EcdsaAdaptorSignature, SecretKey, Signature, SECP256K1};
use std::collections::HashMap;
use std::iter::FromIterator;

/// In satoshi per vbyte.
const SATS_PER_VBYTE: f64 = 1.0;

pub(super) fn lock_transaction(
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
pub(super) struct CommitTransaction {
    inner: Transaction,
    descriptor: Descriptor<PublicKey>,
    amount: Amount,
    sighash: SigHash,
    lock_descriptor: Descriptor<PublicKey>,
    fee: u64,
}

impl CommitTransaction {
    /// Expected size of signed transaction in virtual bytes, plus a
    /// buffer to account for different signature lengths.
    const SIGNED_VBYTES: f64 = 148.5 + (3.0 * 2.0) / 4.0;

    pub(super) fn new(
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
        let fee = (Self::SIGNED_VBYTES * SATS_PER_VBYTE as f64) as u64;

        let commit_tx_amount = lock_amount - fee as u64;
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

    pub(super) fn encsign(
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

    pub(super) fn into_inner(self) -> Transaction {
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
        self.fee
    }
}

#[derive(Debug, Clone)]
pub(super) struct ContractExecutionTransaction {
    inner: Transaction,
    msg_nonce_pairs: Vec<(Vec<u8>, schnorrsig::PublicKey)>,
    sighash: SigHash,
    commit_descriptor: Descriptor<PublicKey>,
}

impl ContractExecutionTransaction {
    /// Expected size of signed transaction in virtual bytes, plus a
    /// buffer to account for different signature lengths.
    const SIGNED_VBYTES: f64 = 206.5 + (3.0 * 2.0) / 4.0;

    pub(super) fn new(
        commit_tx: &CommitTransaction,
        payout: Payout,
        maker_address: &Address,
        taker_address: &Address,
        relative_timelock_in_blocks: u32,
    ) -> Result<Self> {
        let msg_nonce_pairs = payout.msg_nonce_pairs.clone();
        let commit_input = TxIn {
            previous_output: commit_tx.outpoint(),
            sequence: relative_timelock_in_blocks,
            ..Default::default()
        };

        let mut fee = Self::SIGNED_VBYTES * SATS_PER_VBYTE;
        fee += commit_tx.fee() as f64;
        let output = payout
            .with_updated_fee(
                Amount::from_sat(fee as u64),
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
            msg_nonce_pairs,
            sighash,
            commit_descriptor: commit_tx.descriptor(),
        })
    }

    pub(super) fn encsign(
        &self,
        sk: SecretKey,
        oracle_pk: &schnorrsig::PublicKey,
    ) -> Result<EcdsaAdaptorSignature> {
        let adaptor_point = compute_adaptor_point(oracle_pk, &self.msg_nonce_pairs)?;

        Ok(EcdsaAdaptorSignature::encrypt(
            SECP256K1,
            &self.sighash.to_message(),
            &sk,
            &adaptor_point,
        ))
    }

    pub(super) fn into_inner(self) -> Transaction {
        self.inner
    }
}

#[derive(Debug, Clone)]
pub(super) struct RefundTransaction {
    inner: Transaction,
    sighash: SigHash,
    commit_output_descriptor: Descriptor<PublicKey>,
}

impl RefundTransaction {
    /// Expected size of signed transaction in virtual bytes, plus a
    /// buffer to account for different signature lengths.
    const SIGNED_VBYTES: f64 = 206.5 + (3.0 * 2.0) / 4.0;

    pub(super) fn new(
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

    pub(super) fn sighash(&self) -> SigHash {
        self.sighash
    }

    pub(super) fn into_inner(self) -> Transaction {
        self.inner
    }
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
        .witness
        .iter()
        .filter_map(|elem| {
            let elem = elem.as_slice();
            Signature::from_der(&elem[..elem.len() - 1]).ok()
        })
        .find_map(|sig| encsig.recover(SECP256K1, &sig, &pub_them_pk.key).ok())
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
