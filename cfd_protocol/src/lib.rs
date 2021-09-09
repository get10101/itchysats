use anyhow::{bail, Context, Result};
use bdk::bitcoin::hashes::hex::ToHex;
use bdk::bitcoin::hashes::*;
use bdk::bitcoin::util::bip143::SigHashCache;
use bdk::bitcoin::util::psbt::{Global, PartiallySignedTransaction};
use bdk::bitcoin::{
    Address, Amount, OutPoint, PublicKey, SigHash, SigHashType, Transaction, TxIn, TxOut,
};
use bdk::database::BatchDatabase;
use bdk::descriptor::Descriptor;
use bdk::miniscript::descriptor::Wsh;
use bdk::miniscript::DescriptorTrait;
use bdk::wallet::AddressIndex;
use bdk::FeeRate;
use itertools::Itertools;
use secp256k1_zkp::bitcoin_hashes::sha256;
use secp256k1_zkp::{self, schnorrsig, EcdsaAdaptorSignature, SecretKey, Signature, SECP256K1};
use std::collections::HashMap;
use std::iter::FromIterator;

/// In satoshi per vbyte.
const SATS_PER_VBYTE: f64 = 1.0;

/// Static script to be used to create lock tx
const DUMMY_2OF2_MULITISIG: &str =
    "0020b5aa99ed7e0fa92483eb045ab8b7a59146d4d9f6653f21ba729b4331895a5b46";

pub trait WalletExt {
    fn build_party_params(&self, amount: Amount, identity_pk: PublicKey) -> Result<PartyParams>;
}

impl<B, D> WalletExt for bdk::Wallet<B, D>
where
    D: BatchDatabase,
{
    fn build_party_params(&self, amount: Amount, identity_pk: PublicKey) -> Result<PartyParams> {
        let mut builder = self.build_tx();
        builder
            .ordering(bdk::wallet::tx_builder::TxOrdering::Bip69Lexicographic)
            .fee_rate(FeeRate::from_sat_per_vb(1.0))
            .add_recipient(
                DUMMY_2OF2_MULITISIG
                    .parse()
                    .expect("Should be valid script"),
                amount.as_sat(),
            );
        let (lock_psbt, _) = builder.finish()?;
        let address = self.get_address(AddressIndex::New)?.address;
        Ok(PartyParams {
            lock_psbt,
            identity_pk,
            lock_amount: amount,
            address,
        })
    }
}

pub fn build_cfd_transactions(
    (maker, maker_punish_params): (PartyParams, PunishParams),
    (taker, taker_punish_params): (PartyParams, PunishParams),
    oracle_params: OracleParams,
    refund_timelock: u32,
    payouts: Vec<Payout>,
    identity_sk: SecretKey,
) -> Result<CfdTransactions> {
    /// Relative timelock used for every CET.
    ///
    /// This is used to allow parties to punish the publication of revoked commitment transactions.
    ///
    /// TODO: Should this be an argument to this function?
    const CET_TIMELOCK: u32 = 12;

    let lock_tx = lock_transaction(
        maker.lock_psbt.clone(),
        taker.lock_psbt.clone(),
        maker.identity_pk,
        taker.identity_pk,
        maker.lock_amount + taker.lock_amount,
    );

    let commit_tx = CommitTransaction::new(
        &lock_tx.global.unsigned_tx,
        (
            maker.identity_pk,
            maker_punish_params.revocation_pk,
            maker_punish_params.publish_pk,
        ),
        (
            taker.identity_pk,
            taker_punish_params.revocation_pk,
            taker_punish_params.publish_pk,
        ),
    )
    .context("cannot build commit tx")?;

    let identity_pk = secp256k1_zkp::PublicKey::from_secret_key(SECP256K1, &identity_sk);
    let commit_encsig = if identity_pk == maker.identity_pk.key {
        commit_tx.encsign(identity_sk, &taker_punish_params.publish_pk)
    } else if identity_pk == taker.identity_pk.key {
        commit_tx.encsign(identity_sk, &maker_punish_params.publish_pk)
    } else {
        bail!("identity sk does not belong to taker or maker")
    };

    let refund = {
        let tx = RefundTransaction::new(
            &commit_tx,
            refund_timelock,
            &maker.address,
            &taker.address,
            maker.lock_amount,
            taker.lock_amount,
        );

        let sighash = tx.sighash().to_message();
        let sig = SECP256K1.sign(&sighash, &identity_sk);

        (tx.inner, sig)
    };

    let cets = payouts
        .into_iter()
        .map(|payout| {
            let message = payout.message.clone();
            let cet = ContractExecutionTransaction::new(
                &commit_tx,
                payout,
                &maker.address,
                &taker.address,
                CET_TIMELOCK,
            )?;

            let encsig = cet.encsign(identity_sk, &oracle_params.pk, &oracle_params.nonce_pk)?;

            Ok((cet.inner, encsig, message))
        })
        .collect::<Result<Vec<_>>>()
        .context("cannot build and sign all cets")?;

    Ok(CfdTransactions {
        lock: lock_tx,
        commit: (commit_tx.inner, commit_encsig),
        cets,
        refund,
    })
}

pub fn lock_descriptor(maker_pk: PublicKey, taker_pk: PublicKey) -> Descriptor<PublicKey> {
    const MINISCRIPT_TEMPLATE: &str = "c:and_v(v:pk(A),pk_k(B))";

    let maker_pk = ToHex::to_hex(&maker_pk.key);
    let taker_pk = ToHex::to_hex(&taker_pk.key);

    let miniscript = MINISCRIPT_TEMPLATE
        .replace("A", &maker_pk)
        .replace("B", &taker_pk);

    let miniscript = miniscript.parse().expect("a valid miniscript");

    Descriptor::Wsh(Wsh::new(miniscript).expect("a valid descriptor"))
}

pub fn commit_descriptor(
    (maker_own_pk, maker_rev_pk, maker_publish_pk): (PublicKey, PublicKey, PublicKey),
    (taker_own_pk, taker_rev_pk, taker_publish_pk): (PublicKey, PublicKey, PublicKey),
) -> Descriptor<PublicKey> {
    let maker_own_pk_hash = maker_own_pk.pubkey_hash().as_hash();
    let maker_own_pk = (&maker_own_pk.key.serialize().to_vec()).to_hex();
    let maker_publish_pk_hash = maker_publish_pk.pubkey_hash().as_hash();
    let maker_rev_pk_hash = maker_rev_pk.pubkey_hash().as_hash();

    let taker_own_pk_hash = taker_own_pk.pubkey_hash().as_hash();
    let taker_own_pk = (&taker_own_pk.key.serialize().to_vec()).to_hex();
    let taker_publish_pk_hash = taker_publish_pk.pubkey_hash().as_hash();
    let taker_rev_pk_hash = taker_rev_pk.pubkey_hash().as_hash();

    // raw script: or(and(pk(maker_own_pk),pk(taker_own_pk)),or(and(pk(maker_own_pk),and(pk(taker_publish_pk),pk(taker_rev_pk))),and(pk(taker_own_pk),and(pk(maker_publish_pk),pk(maker_rev_pk)))))
    let full_script = format!("wsh(c:andor(pk({maker_own_pk}),pk_k({taker_own_pk}),or_i(and_v(v:pkh({maker_own_pk_hash}),and_v(v:pkh({taker_publish_pk_hash}),pk_h({taker_rev_pk_hash}))),and_v(v:pkh({taker_own_pk_hash}),and_v(v:pkh({maker_publish_pk_hash}),pk_h({maker_rev_pk_hash}))))))",
        maker_own_pk = maker_own_pk,
        taker_own_pk = taker_own_pk,
        maker_own_pk_hash = maker_own_pk_hash,
        taker_own_pk_hash = taker_own_pk_hash,
        taker_publish_pk_hash = taker_publish_pk_hash,
        taker_rev_pk_hash = taker_rev_pk_hash,
        maker_publish_pk_hash = maker_publish_pk_hash,
        maker_rev_pk_hash = maker_rev_pk_hash
    );

    full_script.parse().expect("a valid miniscript")
}

pub fn spending_tx_sighash(
    spending_tx: &Transaction,
    spent_descriptor: &Descriptor<PublicKey>,
    spent_amount: Amount,
) -> secp256k1_zkp::Message {
    let sighash = SigHashCache::new(spending_tx).signature_hash(
        0,
        &spent_descriptor.script_code(),
        spent_amount.as_sat(),
        SigHashType::All,
    );
    sighash.to_message()
}

pub fn finalize_spend_transaction(
    mut tx: Transaction,
    spent_descriptor: &Descriptor<PublicKey>,
    (maker_pk, maker_sig): (PublicKey, Signature),
    (taker_pk, taker_sig): (PublicKey, Signature),
) -> Result<Transaction> {
    let satisfier = HashMap::from_iter(vec![
        (maker_pk, (maker_sig, SigHashType::All)),
        (taker_pk, (taker_sig, SigHashType::All)),
    ]);

    let input = tx
        .input
        .iter_mut()
        .exactly_one()
        .expect("all spend transactions to have one input");
    spent_descriptor.satisfy(input, satisfier)?;

    Ok(tx)
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

    let commit_vout = revoked_commit_tx
        .output
        .iter()
        .position(|out| out.script_pubkey == commit_descriptor.script_pubkey())
        .expect("to find commit output in commit tx");
    let commit_amount = revoked_commit_tx.output[commit_vout].value;

    let mut punish_tx = {
        let txid = revoked_commit_tx.txid();

        let previous_output = OutPoint {
            txid,
            vout: commit_vout as u32,
        };

        let output = TxOut {
            value: commit_amount,
            script_pubkey: address.script_pubkey(),
        };
        let mut tx = Transaction {
            version: 2,
            lock_time: 0,
            input: vec![TxIn {
                previous_output,
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

// NOTE: We have decided to not offer any verification utility because
// the APIs would be incredibly thin

#[derive(Clone)]
pub struct PartyParams {
    pub lock_psbt: PartiallySignedTransaction,
    pub identity_pk: PublicKey,
    pub lock_amount: Amount,
    pub address: Address,
}

pub struct PunishParams {
    pub revocation_pk: PublicKey,
    pub publish_pk: PublicKey,
}

pub struct OracleParams {
    pub pk: schnorrsig::PublicKey,
    pub nonce_pk: schnorrsig::PublicKey,
}

pub struct CfdTransactions {
    pub lock: PartiallySignedTransaction,
    pub commit: (Transaction, EcdsaAdaptorSignature),
    pub cets: Vec<(Transaction, EcdsaAdaptorSignature, Vec<u8>)>,
    pub refund: (Transaction, Signature),
}

#[derive(Debug, Clone)]
pub struct Payout {
    message: Vec<u8>,
    maker_amount: Amount,
    taker_amount: Amount,
}

impl Payout {
    pub fn new(message: Vec<u8>, maker_amount: Amount, taker_amount: Amount) -> Self {
        Self {
            message,
            maker_amount,
            taker_amount,
        }
    }

    fn into_txouts(self, maker_address: &Address, taker_address: &Address) -> Vec<TxOut> {
        let txouts = [
            (self.maker_amount, maker_address),
            (self.taker_amount, taker_address),
        ]
        .iter()
        .filter_map(|(amount, address)| {
            let script_pubkey = address.script_pubkey();
            let dust_limit = script_pubkey.dust_value();
            (amount >= &dust_limit).then(|| TxOut {
                value: amount.as_sat(),
                script_pubkey,
            })
        })
        .collect::<Vec<_>>();

        txouts
    }

    /// Subtracts fee fairly from both outputs
    ///
    /// We need to consider a few cases:
    /// - If both amounts are >= DUST, they share the fee equally
    /// - If one amount is < DUST, it set to 0 and the other output needs to cover for the fee.
    fn with_updated_fee(
        self,
        fee: Amount,
        dust_limit_maker: Amount,
        dust_limit_taker: Amount,
    ) -> Result<Self> {
        let maker_amount = self.maker_amount;
        let taker_amount = self.taker_amount;

        let mut updated = self;
        match (
            maker_amount
                .checked_sub(fee / 2)
                .map(|a| a > dust_limit_maker)
                .unwrap_or(false),
            taker_amount
                .checked_sub(fee / 2)
                .map(|a| a > dust_limit_taker)
                .unwrap_or(false),
        ) {
            (true, true) => {
                updated.maker_amount -= fee / 2;
                updated.taker_amount -= fee / 2;
            }
            (false, true) => {
                updated.maker_amount = Amount::ZERO;
                updated.taker_amount = taker_amount - (fee + maker_amount);
            }
            (true, false) => {
                updated.maker_amount = maker_amount - (fee + taker_amount);
                updated.taker_amount = Amount::ZERO;
            }
            (false, false) => bail!("Amounts are too small, could not subtract fee."),
        }
        Ok(updated)
    }
}

const BIP340_MIDSTATE: [u8; 32] = [
    0x9c, 0xec, 0xba, 0x11, 0x23, 0x92, 0x53, 0x81, 0x11, 0x67, 0x91, 0x12, 0xd1, 0x62, 0x7e, 0x0f,
    0x97, 0xc8, 0x75, 0x50, 0x00, 0x3c, 0xc7, 0x65, 0x90, 0xf6, 0x11, 0x64, 0x33, 0xe9, 0xb6, 0x6a,
];

sha256t_hash_newtype!(
    BIP340Hash,
    BIP340HashTag,
    BIP340_MIDSTATE,
    64,
    doc = "bip340 hash",
    true
);

/// Compute a signature point for the given oracle public key, announcement nonce public key and message.
pub fn compute_signature_point(
    oracle_pk: &schnorrsig::PublicKey,
    nonce_pk: &schnorrsig::PublicKey,
    msg: &[u8],
) -> Result<secp256k1_zkp::PublicKey> {
    fn schnorr_pubkey_to_pubkey(pk: &schnorrsig::PublicKey) -> Result<secp256k1_zkp::PublicKey> {
        let mut buf = Vec::<u8>::with_capacity(33);
        buf.push(0x02);
        buf.extend(&pk.serialize());
        Ok(secp256k1_zkp::PublicKey::from_slice(&buf)?)
    }

    let hash = {
        let mut buf = Vec::<u8>::new();
        buf.extend(&nonce_pk.serialize());
        buf.extend(&oracle_pk.serialize());
        buf.extend(
            secp256k1_zkp::Message::from_hashed_data::<sha256::Hash>(msg)
                .as_ref()
                .to_vec(),
        );
        BIP340Hash::hash(&buf).into_inner().to_vec()
    };
    let mut oracle_pk = schnorr_pubkey_to_pubkey(oracle_pk)?;
    oracle_pk.mul_assign(SECP256K1, &hash)?;
    let nonce_pk = schnorr_pubkey_to_pubkey(nonce_pk)?;
    Ok(nonce_pk.combine(&oracle_pk)?)
}

#[derive(Debug, Clone)]
struct ContractExecutionTransaction {
    inner: Transaction,
    message: Vec<u8>,
    sighash: SigHash,
    commit_descriptor: Descriptor<PublicKey>,
}

impl ContractExecutionTransaction {
    /// Expected size of signed transaction in virtual bytes, plus a
    /// buffer to account for different signature lengths.
    const SIGNED_VBYTES: f64 = 206.5 + (3.0 * 2.0) / 4.0;

    fn new(
        commit_tx: &CommitTransaction,
        payout: Payout,
        maker_address: &Address,
        taker_address: &Address,
        relative_timelock_in_blocks: u32,
    ) -> Result<Self> {
        let message = payout.message.clone();

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
            message,
            sighash,
            commit_descriptor: commit_tx.descriptor(),
        })
    }

    fn encsign(
        &self,
        sk: SecretKey,
        oracle_pk: &schnorrsig::PublicKey,
        nonce_pk: &schnorrsig::PublicKey,
    ) -> Result<EcdsaAdaptorSignature> {
        let signature_point = compute_signature_point(oracle_pk, nonce_pk, &self.message)?;

        Ok(EcdsaAdaptorSignature::encrypt(
            SECP256K1,
            &self.sighash.to_message(),
            &sk,
            &signature_point,
        ))
    }
}

#[derive(Debug, Clone)]
struct RefundTransaction {
    inner: Transaction,
    sighash: SigHash,
    commit_output_descriptor: Descriptor<PublicKey>,
}

impl RefundTransaction {
    /// Expected size of signed transaction in virtual bytes, plus a
    /// buffer to account for different signature lengths.
    const SIGNED_VBYTES: f64 = 206.5 + (3.0 * 2.0) / 4.0;

    fn new(
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

    fn sighash(&self) -> SigHash {
        self.sighash
    }
}

#[derive(Debug, Clone)]
struct CommitTransaction {
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

    fn new(
        lock_tx: &Transaction,
        (maker_pk, maker_rev_pk, maker_publish_pk): (PublicKey, PublicKey, PublicKey),
        (taker_pk, taker_rev_pk, taker_publish_pk): (PublicKey, PublicKey, PublicKey),
    ) -> Result<Self> {
        let lock_descriptor = lock_descriptor(maker_pk, taker_pk);
        let (lock_outpoint, lock_amount) = {
            let vout = lock_tx
                .output
                .iter()
                .position(|out| out.script_pubkey == lock_descriptor.script_pubkey())
                .context("lock script not found in lock tx")?;
            let outpoint = OutPoint {
                txid: lock_tx.txid(),
                vout: vout as u32,
            };
            let amount = lock_tx.output[vout].value;

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

    fn encsign(&self, sk: SecretKey, publish_them_pk: &PublicKey) -> EcdsaAdaptorSignature {
        EcdsaAdaptorSignature::encrypt(
            SECP256K1,
            &self.sighash.to_message(),
            &sk,
            &publish_them_pk.key,
        )
    }

    fn outpoint(&self) -> OutPoint {
        let txid = self.inner.txid();
        let vout = self
            .inner
            .output
            .iter()
            .position(|out| out.script_pubkey == self.descriptor.script_pubkey())
            .expect("to find commit output in commit tx");

        OutPoint {
            txid,
            vout: vout as u32,
        }
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

fn lock_transaction(
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
            out.script_pubkey != DUMMY_2OF2_MULITISIG.parse().expect("To be a valid script")
        })
        .collect::<Vec<_>>();

    let taker_change = taker_psbt
        .global
        .unsigned_tx
        .output
        .into_iter()
        .filter(|out| {
            out.script_pubkey != DUMMY_2OF2_MULITISIG.parse().expect("To be a valid script")
        })
        .collect();

    let lock_output = TxOut {
        value: amount.as_sat(),
        script_pubkey: lock_descriptor.script_pubkey(),
    };

    let lock_tx = Transaction {
        version: 2,
        lock_time: 0,
        input: vec![
            maker_psbt.global.unsigned_tx.input,
            taker_psbt.global.unsigned_tx.input,
        ]
        .concat(),
        output: vec![vec![lock_output], maker_change, taker_change].concat(),
    };

    PartiallySignedTransaction {
        global: Global::from_unsigned_tx(lock_tx).expect("to be unsigned"),
        inputs: vec![maker_psbt.inputs, taker_psbt.inputs].concat(),
        outputs: vec![maker_psbt.outputs, taker_psbt.outputs].concat(),
    }
}

pub trait TransactionExt {
    fn get_virtual_size(&self) -> f64;
}

impl TransactionExt for Transaction {
    fn get_virtual_size(&self) -> f64 {
        self.get_weight() as f64 / 4.0
    }
}

trait SigHashExt {
    fn to_message(self) -> secp256k1_zkp::Message;
}

impl SigHashExt for SigHash {
    fn to_message(self) -> secp256k1_zkp::Message {
        use secp256k1_zkp::bitcoin_hashes::Hash;
        let hash = secp256k1_zkp::bitcoin_hashes::sha256d::Hash::from_inner(*self.as_inner());

        hash.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use bdk::bitcoin::Network;

    // TODO add proptest for this

    #[test]
    fn test_fee_subtraction_bigger_than_dust() {
        let key = "032e58afe51f9ed8ad3cc7897f634d881fdbe49a81564629ded8156bebd2ffd1af"
            .parse()
            .unwrap();
        let dummy_address = Address::p2wpkh(&key, Network::Regtest).unwrap();
        let dummy_dust_limit = dummy_address.script_pubkey().dust_value();

        let orig_maker_amount = 1000;
        let orig_taker_amount = 1000;
        let payout = Payout::new(
            b"win".to_vec(),
            Amount::from_sat(orig_maker_amount),
            Amount::from_sat(orig_taker_amount),
        );
        let fee = 100;
        let updated_payout = payout
            .with_updated_fee(Amount::from_sat(fee), dummy_dust_limit, dummy_dust_limit)
            .unwrap();

        assert_eq!(
            updated_payout.maker_amount,
            Amount::from_sat(orig_maker_amount - fee / 2)
        );
        assert_eq!(
            updated_payout.taker_amount,
            Amount::from_sat(orig_taker_amount - fee / 2)
        );
    }

    #[test]
    fn test_fee_subtraction_smaller_than_dust() {
        let key = "032e58afe51f9ed8ad3cc7897f634d881fdbe49a81564629ded8156bebd2ffd1af"
            .parse()
            .unwrap();
        let dummy_address = Address::p2wpkh(&key, Network::Regtest).unwrap();
        let dummy_dust_limit = dummy_address.script_pubkey().dust_value();

        let orig_maker_amount = dummy_dust_limit.as_sat() - 1;
        let orig_taker_amount = 1000;
        let payout = Payout::new(
            b"win".to_vec(),
            Amount::from_sat(orig_maker_amount),
            Amount::from_sat(orig_taker_amount),
        );
        let fee = 100;
        let updated_payout = payout
            .with_updated_fee(Amount::from_sat(fee), dummy_dust_limit, dummy_dust_limit)
            .unwrap();

        assert_eq!(updated_payout.maker_amount, Amount::from_sat(0));
        assert_eq!(
            updated_payout.taker_amount,
            Amount::from_sat(orig_taker_amount - (fee + orig_maker_amount))
        );
    }
}
