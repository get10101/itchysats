use anyhow::{bail, Context, Result};
use bdk::bitcoin::hashes::*;
use bdk::bitcoin::Script;

use bdk::bitcoin::hashes::hex::ToHex;
use bdk::bitcoin::util::bip143::SigHashCache;
use bdk::bitcoin::util::psbt::{Global, PartiallySignedTransaction};
use bdk::bitcoin::{
    self, Address, Amount, Network, OutPoint, PublicKey, SigHash, SigHashType, Transaction, TxIn,
    TxOut,
};
use bdk::database::BatchDatabase;
use bdk::descriptor::Descriptor;
use bdk::miniscript::descriptor::Wsh;
use bdk::miniscript::DescriptorTrait;
use bdk::wallet::AddressIndex;
use bdk::FeeRate;
use bitcoin::PrivateKey;
use itertools::Itertools;
use secp256k1_zkp::{self, schnorrsig, EcdsaAdaptorSignature, SecretKey, Signature, SECP256K1};
use std::collections::HashMap;

/// In satoshi per vbyte.
const SATS_PER_VBYTE: f64 = 1.0;

/// In satoshi.
///
/// FIXME: Use Script::dust_value instead.
const P2PKH_DUST_LIMIT: u64 = 546;

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
            .add_recipient(Script::new(), amount.as_sat());
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

    let lock_tx = LockTransaction::new(
        maker.lock_psbt.clone(),
        taker.lock_psbt.clone(),
        maker.identity_pk,
        taker.identity_pk,
        maker.lock_amount + taker.lock_amount,
    )
    .context("cannot build lock tx")?;

    let commit_tx = CommitTransaction::new(
        &lock_tx,
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
        commit_tx.encsign(identity_sk, &taker_punish_params.publish_pk)?
    } else if identity_pk == taker.identity_pk.key {
        commit_tx.encsign(identity_sk, &maker_punish_params.publish_pk)?
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

        let sighash =
            secp256k1_zkp::Message::from_slice(&tx.sighash()).expect("sighash is valid message");
        let sig = SECP256K1.sign(&sighash, &identity_sk);

        (tx.inner, sig)
    };

    let cets = payouts
        .iter()
        .map(|payout| {
            let cet = ContractExecutionTransaction::new(
                &commit_tx,
                payout,
                &maker.address,
                &taker.address,
                CET_TIMELOCK,
            )?;

            let encsig = cet.encsign(identity_sk, &oracle_params.pk, &oracle_params.nonce_pk)?;

            Ok((cet.inner, encsig, payout.message))
        })
        .collect::<Result<Vec<_>>>()
        .context("cannot build and sign all cets")?;

    Ok(CfdTransactions {
        lock: lock_tx.inner,
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
    secp256k1_zkp::Message::from_slice(&sighash).expect("sighash is valid message")
}

pub fn finalize_spend_transaction(
    mut tx: Transaction,
    spent_descriptor: &Descriptor<PublicKey>,
    (maker_pk, maker_sig): (PublicKey, Signature),
    (taker_pk, taker_sig): (PublicKey, Signature),
) -> Result<Transaction> {
    let satisfier = {
        let mut satisfier = HashMap::with_capacity(2);

        satisfier.insert(maker_pk, (maker_sig, SigHashType::All));
        satisfier.insert(taker_pk, (taker_sig, SigHashType::All));

        satisfier
    };

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
    publish_them_pk: PublicKey,
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
            bitcoin::secp256k1::Signature::from_der(&elem[..elem.len() - 1]).ok()
        })
        .find_map(|sig| encsig.recover(SECP256K1, &sig, &publish_them_pk.key).ok())
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

    let digest = SigHashCache::new(&punish_tx).signature_hash(
        0,
        &commit_descriptor.script_code(),
        commit_amount,
        SigHashType::All,
    );

    let satisfier = {
        let mut satisfier = HashMap::with_capacity(3);

        let pk = bitcoin::secp256k1::PublicKey::from_secret_key(SECP256K1, &sk);
        let pk = bitcoin::PublicKey {
            compressed: true,
            key: pk,
        };
        let pk_hash = pk.pubkey_hash();
        let sig_sk = SECP256K1.sign(&secp256k1_zkp::Message::from_slice(&digest)?, &sk);

        let publish_them_pk_hash = publish_them_pk.pubkey_hash();
        let sig_publish_other = SECP256K1.sign(
            &secp256k1_zkp::Message::from_slice(&digest)?,
            &publish_them_sk,
        );

        let revocation_them_pk = PublicKey::from_private_key(
            SECP256K1,
            &PrivateKey {
                compressed: true,
                network: Network::Regtest,
                key: revocation_them_sk,
            },
        );
        let revocation_them_pk_hash = revocation_them_pk.pubkey_hash();
        let sig_revocation_other = SECP256K1.sign(
            &secp256k1_zkp::Message::from_slice(&digest)?,
            &revocation_them_sk,
        );

        satisfier.insert(pk_hash.as_hash(), (pk, (sig_sk, SigHashType::All)));

        satisfier.insert(
            publish_them_pk_hash.as_hash(),
            (publish_them_pk, (sig_publish_other, SigHashType::All)),
        );
        satisfier.insert(
            revocation_them_pk_hash.as_hash(),
            (revocation_them_pk, (sig_revocation_other, SigHashType::All)),
        );

        satisfier
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
    pub cets: Vec<(Transaction, EcdsaAdaptorSignature, Message)>,
    pub refund: (Transaction, Signature),
}

// NOTE: This is a simplification. Our use-case will not work with
// a simple enumeration of possible messages
#[derive(Debug, Clone, Copy)]
pub struct Payout {
    message: Message,
    maker_amount: Amount,
    taker_amount: Amount,
}

impl Payout {
    pub fn new(message: Message, maker_amount: Amount, taker_amount: Amount) -> Self {
        Self {
            message,
            maker_amount,
            taker_amount,
        }
    }

    fn as_txouts(&self, maker_address: &Address, taker_address: &Address) -> Vec<TxOut> {
        let txouts = [
            (self.maker_amount, maker_address),
            (self.taker_amount, taker_address),
        ]
        .iter()
        .filter_map(|(amount, address)| {
            (amount >= &Amount::from_sat(P2PKH_DUST_LIMIT)).then(|| TxOut {
                value: amount.as_sat(),
                script_pubkey: address.script_pubkey(),
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
    fn with_updated_fee(self, fee: Amount) -> Result<Self> {
        let mut updated = self;
        let dust_limit = Amount::from_sat(P2PKH_DUST_LIMIT);

        match (
            self.maker_amount
                .checked_sub(fee / 2)
                .map(|a| a > dust_limit)
                .unwrap_or(false),
            self.taker_amount
                .checked_sub(fee / 2)
                .map(|a| a > dust_limit)
                .unwrap_or(false),
        ) {
            (true, true) => {
                updated.maker_amount -= fee / 2;
                updated.taker_amount -= fee / 2;
            }
            (false, true) => {
                updated.maker_amount = Amount::ZERO;
                updated.taker_amount = self.taker_amount - (fee + self.maker_amount);
            }
            (true, false) => {
                updated.maker_amount = self.maker_amount - (fee + self.taker_amount);
                updated.taker_amount = Amount::ZERO;
            }
            (false, false) => bail!("Amounts are too small, could not subtract fee."),
        }
        Ok(updated)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Message {
    Win,
    Lose,
}

impl From<Message> for secp256k1_zkp::Message {
    fn from(msg: Message) -> Self {
        // TODO: Tag hash with prefix and other public data
        secp256k1_zkp::Message::from_hashed_data::<secp256k1_zkp::bitcoin_hashes::sha256::Hash>(
            msg.to_string().as_bytes(),
        )
    }
}

impl ToString for Message {
    fn to_string(&self) -> String {
        match self {
            Message::Win => "win".to_string(),
            Message::Lose => "lose".to_string(),
        }
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
fn compute_signature_point(
    oracle_pk: &schnorrsig::PublicKey,
    nonce_pk: &schnorrsig::PublicKey,
    message: Message,
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
        buf.extend(secp256k1_zkp::Message::from(message).as_ref().to_vec());
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
    message: Message,
    sighash: SigHash,
    commit_descriptor: Descriptor<PublicKey>,
}

impl ContractExecutionTransaction {
    /// Expected size of signed transaction in virtual bytes, plus a
    /// buffer to account for different signature lengths.
    const SIGNED_VBYTES: f64 = 175.25 + (3.0 * 2.0) / 4.0;

    fn new(
        commit_tx: &CommitTransaction,
        payout: &Payout,
        maker_address: &Address,
        taker_address: &Address,
        relative_timelock_in_blocks: u32,
    ) -> Result<Self> {
        let commit_input = TxIn {
            previous_output: commit_tx.outpoint(),
            sequence: relative_timelock_in_blocks,
            ..Default::default()
        };

        let mut tx = Transaction {
            version: 2,
            lock_time: 0,
            input: vec![commit_input],
            output: payout.as_txouts(maker_address, taker_address),
        };

        let mut fee = Self::SIGNED_VBYTES * SATS_PER_VBYTE;
        fee += commit_tx.fee() as f64;

        let payout = payout.with_updated_fee(Amount::from_sat(fee as u64))?;
        tx.output = payout.as_txouts(maker_address, taker_address);

        let sighash = SigHashCache::new(&tx).signature_hash(
            0,
            &commit_tx.descriptor.script_code(),
            commit_tx.amount.as_sat(),
            SigHashType::All,
        );

        Ok(Self {
            inner: tx,
            message: payout.message,
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
        let signature_point = compute_signature_point(oracle_pk, nonce_pk, self.message)?;

        Ok(EcdsaAdaptorSignature::encrypt(
            SECP256K1,
            &secp256k1_zkp::Message::from_slice(&self.sighash).expect("sighash is valid message"),
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
        lock_tx: &LockTransaction,
        (maker_own_pk, maker_rev_pk, maker_publish_pk): (PublicKey, PublicKey, PublicKey),
        (taker_own_pk, taker_rev_pk, taker_publish_pk): (PublicKey, PublicKey, PublicKey),
    ) -> Result<Self> {
        let lock_tx_amount = lock_tx.amount().as_sat();

        let lock_input = TxIn {
            previous_output: lock_tx.lock_outpoint(),
            ..Default::default()
        };

        let descriptor = commit_descriptor(
            (maker_own_pk, maker_rev_pk, maker_publish_pk),
            (taker_own_pk, taker_rev_pk, taker_publish_pk),
        );

        let output = TxOut {
            value: lock_tx_amount,
            script_pubkey: descriptor
                .address(Network::Regtest)
                .expect("can derive address from descriptor")
                .script_pubkey(),
        };

        let mut inner = Transaction {
            version: 2,
            lock_time: 0,
            input: vec![lock_input],
            output: vec![output],
        };
        let fee = (Self::SIGNED_VBYTES * SATS_PER_VBYTE as f64) as u64;

        let commit_tx_amount = lock_tx_amount - fee as u64;
        inner.output[0].value = commit_tx_amount;

        let sighash = SigHashCache::new(&inner).signature_hash(
            0,
            &lock_tx.descriptor().script_code(),
            lock_tx.amount().as_sat(),
            SigHashType::All,
        );

        Ok(Self {
            inner,
            descriptor,
            lock_descriptor: lock_tx.descriptor(),
            amount: Amount::from_sat(commit_tx_amount),
            sighash,
            fee,
        })
    }

    fn encsign(&self, sk: SecretKey, publish_them_pk: &PublicKey) -> Result<EcdsaAdaptorSignature> {
        Ok(EcdsaAdaptorSignature::encrypt(
            SECP256K1,
            &secp256k1_zkp::Message::from_slice(&self.sighash).expect("sighash is valid message"),
            &sk,
            &publish_them_pk.key,
        ))
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

#[derive(Debug, Clone)]
struct LockTransaction {
    inner: PartiallySignedTransaction,
    lock_descriptor: Descriptor<PublicKey>,
    amount: Amount,
}

impl LockTransaction {
    fn new(
        maker_psbt: PartiallySignedTransaction,
        taker_psbt: PartiallySignedTransaction,
        maker_pk: PublicKey,
        taker_pk: PublicKey,
        amount: Amount,
    ) -> Result<Self> {
        let lock_descriptor = lock_descriptor(maker_pk, taker_pk);

        let maker_change = maker_psbt
            .global
            .unsigned_tx
            .output
            .into_iter()
            .filter(|out| !out.script_pubkey.is_empty())
            .collect::<Vec<_>>();

        let taker_change = taker_psbt
            .global
            .unsigned_tx
            .output
            .into_iter()
            .filter(|out| !out.script_pubkey.is_empty())
            .collect();

        let lock_output = TxOut {
            value: amount.as_sat(),
            script_pubkey: lock_descriptor
                .address(Network::Regtest)
                .expect("can derive address from descriptor")
                .script_pubkey(),
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

        let inner = PartiallySignedTransaction {
            global: Global::from_unsigned_tx(lock_tx)?,
            inputs: vec![maker_psbt.inputs, taker_psbt.inputs].concat(),
            outputs: vec![maker_psbt.outputs, taker_psbt.outputs].concat(),
        };

        Ok(Self {
            inner,
            lock_descriptor,
            amount,
        })
    }

    fn lock_outpoint(&self) -> OutPoint {
        let txid = self.inner.global.unsigned_tx.txid();
        let vout = self
            .inner
            .global
            .unsigned_tx
            .output
            .iter()
            .position(|out| out.script_pubkey == self.lock_descriptor.script_pubkey())
            .expect("to find lock output in lock tx");

        OutPoint {
            txid,
            vout: vout as u32,
        }
    }

    fn descriptor(&self) -> Descriptor<PublicKey> {
        self.lock_descriptor.clone()
    }

    fn amount(&self) -> Amount {
        self.amount
    }
}

trait TransactionExt {
    fn get_virtual_size(&self) -> f64;
}

impl TransactionExt for bitcoin::Transaction {
    fn get_virtual_size(&self) -> f64 {
        self.get_weight() as f64 / 4.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdk::bitcoin::util::bip32::ExtendedPrivKey;
    use bdk::bitcoin::PrivateKey;
    use bdk::wallet::AddressIndex;
    use bdk::SignOptions;
    use rand::{CryptoRng, RngCore, SeedableRng};
    use rand_chacha::ChaChaRng;

    #[test]
    fn run_cfd_protocol() {
        let mut rng = ChaChaRng::seed_from_u64(0);

        let maker_lock_amount = Amount::ONE_BTC;
        let taker_lock_amount = Amount::ONE_BTC;

        let oracle = Oracle::new(&mut rng);
        let (event, announcement) = announce(&mut rng);

        let (maker_sk, maker_pk) = make_keypair(&mut rng);
        let (taker_sk, taker_pk) = make_keypair(&mut rng);

        let payouts = vec![
            Payout::new(Message::Win, Amount::from_btc(2.0).unwrap(), Amount::ZERO),
            Payout::new(Message::Lose, Amount::ZERO, Amount::from_btc(2.0).unwrap()),
        ];

        let refund_timelock = 0;

        let maker_wallet = build_wallet(&mut rng, Amount::from_btc(0.4).unwrap(), 5).unwrap();
        let taker_wallet = build_wallet(&mut rng, Amount::from_btc(0.4).unwrap(), 5).unwrap();

        let maker_address = maker_wallet.get_address(AddressIndex::New).unwrap();
        let taker_address = taker_wallet.get_address(AddressIndex::New).unwrap();

        let lock_amount = maker_lock_amount + taker_lock_amount;
        let (maker_revocation_sk, maker_revocation_pk) = make_keypair(&mut rng);
        let (maker_publish_sk, maker_publish_pk) = make_keypair(&mut rng);

        let (taker_revocation_sk, taker_revocation_pk) = make_keypair(&mut rng);
        let (taker_publish_sk, taker_publish_pk) = make_keypair(&mut rng);

        let maker_params = maker_wallet
            .build_party_params(maker_lock_amount, maker_pk)
            .unwrap();
        let taker_params = taker_wallet
            .build_party_params(taker_lock_amount, taker_pk)
            .unwrap();

        let maker_cfd_txs = build_cfd_transactions(
            (
                maker_params.clone(),
                PunishParams {
                    revocation_pk: maker_revocation_pk,
                    publish_pk: maker_publish_pk,
                },
            ),
            (
                taker_params.clone(),
                PunishParams {
                    revocation_pk: taker_revocation_pk,
                    publish_pk: taker_publish_pk,
                },
            ),
            OracleParams {
                pk: oracle.public_key(),
                nonce_pk: event.nonce_pk,
            },
            refund_timelock,
            payouts.clone(),
            maker_sk,
        )
        .unwrap();

        let taker_cfd_txs = build_cfd_transactions(
            (
                maker_params,
                PunishParams {
                    revocation_pk: maker_revocation_pk,
                    publish_pk: maker_publish_pk,
                },
            ),
            (
                taker_params,
                PunishParams {
                    revocation_pk: taker_revocation_pk,
                    publish_pk: taker_publish_pk,
                },
            ),
            OracleParams {
                pk: oracle.public_key(),
                nonce_pk: event.nonce_pk,
            },
            refund_timelock,
            payouts,
            taker_sk,
        )
        .unwrap();

        let commit_descriptor = commit_descriptor(
            (maker_pk, maker_revocation_pk, maker_publish_pk),
            (taker_pk, taker_revocation_pk, taker_publish_pk),
        );

        let commit_amount = Amount::from_sat(maker_cfd_txs.commit.0.output[0].value);
        assert_eq!(
            commit_amount.as_sat(),
            taker_cfd_txs.commit.0.output[0].value
        );

        {
            let refund_sighash =
                spending_tx_sighash(&taker_cfd_txs.refund.0, &commit_descriptor, commit_amount);
            SECP256K1
                .verify(&refund_sighash, &maker_cfd_txs.refund.1, &maker_pk.key)
                .expect("valid maker refund sig")
        };

        {
            let refund_sighash =
                spending_tx_sighash(&maker_cfd_txs.refund.0, &commit_descriptor, commit_amount);
            SECP256K1
                .verify(&refund_sighash, &taker_cfd_txs.refund.1, &taker_pk.key)
                .expect("valid taker refund sig")
        };

        // TODO: We should not rely on order
        for (maker_cet, taker_cet) in maker_cfd_txs.cets.iter().zip(taker_cfd_txs.cets.iter()) {
            let cet_sighash = {
                let maker_sighash =
                    spending_tx_sighash(&maker_cet.0, &commit_descriptor, commit_amount);
                let taker_sighash =
                    spending_tx_sighash(&taker_cet.0, &commit_descriptor, commit_amount);

                assert_eq!(maker_sighash, taker_sighash);
                maker_sighash
            };

            let encryption_point = {
                let maker_encryption_point = compute_signature_point(
                    &oracle.public_key(),
                    &announcement.nonce_pk(),
                    maker_cet.2,
                )
                .unwrap();
                let taker_encryption_point = compute_signature_point(
                    &oracle.public_key(),
                    &announcement.nonce_pk(),
                    taker_cet.2,
                )
                .unwrap();

                assert_eq!(maker_encryption_point, taker_encryption_point);
                maker_encryption_point
            };

            let maker_encsig = maker_cet.1;
            maker_encsig
                .verify(SECP256K1, &cet_sighash, &maker_pk.key, &encryption_point)
                .expect("valid maker cet encsig");

            let taker_encsig = taker_cet.1;
            taker_encsig
                .verify(SECP256K1, &cet_sighash, &taker_pk.key, &encryption_point)
                .expect("valid taker cet encsig");
        }

        let lock_descriptor = lock_descriptor(maker_pk, taker_pk);

        {
            let commit_sighash =
                spending_tx_sighash(&maker_cfd_txs.commit.0, &lock_descriptor, lock_amount);
            let commit_encsig = maker_cfd_txs.commit.1;
            commit_encsig
                .verify(
                    SECP256K1,
                    &commit_sighash,
                    &maker_pk.key,
                    &taker_publish_pk.key,
                )
                .expect("valid maker commit encsig");
        };

        {
            let commit_sighash =
                spending_tx_sighash(&taker_cfd_txs.commit.0, &lock_descriptor, lock_amount);
            let commit_encsig = taker_cfd_txs.commit.1;
            commit_encsig
                .verify(
                    SECP256K1,
                    &commit_sighash,
                    &taker_pk.key,
                    &maker_publish_pk.key,
                )
                .expect("valid taker commit encsig");
        };

        // sign lock transaction

        let mut signed_lock_tx = maker_cfd_txs.lock;
        maker_wallet
            .sign(
                &mut signed_lock_tx,
                SignOptions {
                    trust_witness_utxo: true,
                    ..Default::default()
                },
            )
            .unwrap();

        taker_wallet
            .sign(
                &mut signed_lock_tx,
                SignOptions {
                    trust_witness_utxo: true,
                    ..Default::default()
                },
            )
            .unwrap();

        let signed_lock_tx = signed_lock_tx.extract_tx();

        // verify commit transaction

        let commit_tx = maker_cfd_txs.commit.0;
        let maker_sig = maker_cfd_txs.commit.1.decrypt(&taker_publish_sk).unwrap();
        let taker_sig = taker_cfd_txs.commit.1.decrypt(&maker_publish_sk).unwrap();
        let signed_commit_tx = finalize_spend_transaction(
            commit_tx,
            &lock_descriptor,
            (maker_pk, maker_sig),
            (taker_pk, taker_sig),
        )
        .unwrap();

        check_tx_fee(&[&signed_lock_tx], &signed_commit_tx).expect("correct fees for commit tx");

        lock_descriptor
            .address(Network::Regtest)
            .expect("can derive address from descriptor")
            .script_pubkey()
            .verify(
                0,
                lock_amount.as_sat(),
                bitcoin::consensus::serialize(&signed_commit_tx).as_slice(),
            )
            .expect("valid signed commit transaction");

        // verify refund transaction

        let maker_sig = maker_cfd_txs.refund.1;
        let taker_sig = taker_cfd_txs.refund.1;
        let signed_refund_tx = finalize_spend_transaction(
            maker_cfd_txs.refund.0,
            &commit_descriptor,
            (maker_pk, maker_sig),
            (taker_pk, taker_sig),
        )
        .unwrap();

        check_tx_fee(&[&signed_commit_tx], &signed_refund_tx).expect("correct fees for refund tx");

        commit_descriptor
            .address(Network::Regtest)
            .expect("can derive address from descriptor")
            .script_pubkey()
            .verify(
                0,
                commit_amount.as_sat(),
                bitcoin::consensus::serialize(&signed_refund_tx).as_slice(),
            )
            .expect("valid signed refund transaction");

        // verify cets

        let attestations = [Message::Win, Message::Lose]
            .iter()
            .map(|msg| (*msg, oracle.attest(&event, *msg)))
            .collect::<HashMap<_, _>>();

        maker_cfd_txs
            .cets
            .into_iter()
            .zip(taker_cfd_txs.cets)
            .try_for_each(|((cet, maker_encsig, msg), (_, taker_encsig, _))| {
                let oracle_sig = attestations
                    .get(&msg)
                    .expect("oracle to sign all messages in test");
                let (_nonce_pk, signature_scalar) = schnorrsig_decompose(oracle_sig);

                let maker_sig = maker_encsig
                    .decrypt(&signature_scalar)
                    .context("could not decrypt maker encsig on cet")?;
                let taker_sig = taker_encsig
                    .decrypt(&signature_scalar)
                    .context("could not decrypt taker encsig on cet")?;

                let signed_cet = finalize_spend_transaction(
                    cet,
                    &commit_descriptor,
                    (maker_pk, maker_sig),
                    (taker_pk, taker_sig),
                )?;

                check_tx_fee(&[&signed_commit_tx], &signed_cet).expect("correct fees for cet");

                commit_descriptor
                    .address(Network::Regtest)
                    .expect("can derive address from descriptor")
                    .script_pubkey()
                    .verify(
                        0,
                        commit_amount.as_sat(),
                        bitcoin::consensus::serialize(&signed_cet).as_slice(),
                    )
                    .context("failed to verify cet")
            })
            .expect("all cets to be properly signed");

        // verify punishment transactions

        let punish_tx = punish_transaction(
            &commit_descriptor,
            &maker_address,
            maker_cfd_txs.commit.1,
            maker_sk,
            taker_revocation_sk,
            taker_publish_pk,
            &signed_commit_tx,
        )
        .unwrap();

        check_tx_fee(&[&signed_commit_tx], &punish_tx).expect("correct fees for punish tx");

        commit_descriptor
            .address(Network::Regtest)
            .expect("can derive address from descriptor")
            .script_pubkey()
            .verify(
                0,
                commit_amount.as_sat(),
                bitcoin::consensus::serialize(&punish_tx).as_slice(),
            )
            .expect("valid punish transaction signed by maker");

        let punish_tx = punish_transaction(
            &commit_descriptor,
            &taker_address,
            taker_cfd_txs.commit.1,
            taker_sk,
            maker_revocation_sk,
            maker_publish_pk,
            &signed_commit_tx,
        )
        .unwrap();

        commit_descriptor
            .address(Network::Regtest)
            .expect("can derive address from descriptor")
            .script_pubkey()
            .verify(
                0,
                commit_amount.as_sat(),
                bitcoin::consensus::serialize(&punish_tx).as_slice(),
            )
            .expect("valid punish transaction signed by taker");
    }

    #[test]
    fn test_fee_subtraction_bigger_than_dust() {
        let orig_maker_amount = 1000;
        let orig_taker_amount = 1000;
        let payout = Payout::new(
            Message::Win,
            Amount::from_sat(orig_maker_amount),
            Amount::from_sat(orig_taker_amount),
        );
        let fee = 100;
        let updated_payout = payout.with_updated_fee(Amount::from_sat(fee)).unwrap();

        assert_eq!(
            updated_payout.maker_amount,
            Amount::from_sat(orig_maker_amount - fee / 2)
        );
        assert_eq!(
            updated_payout.taker_amount,
            Amount::from_sat(orig_taker_amount - fee / 2)
        );
    }

    // TODO add proptest for this
    #[test]
    fn test_fee_subtraction_smaller_than_dust() {
        let orig_maker_amount = P2PKH_DUST_LIMIT - 1;
        let orig_taker_amount = 1000;
        let payout = Payout::new(
            Message::Win,
            Amount::from_sat(orig_maker_amount),
            Amount::from_sat(orig_taker_amount),
        );
        let fee = 100;
        let updated_payout = payout.with_updated_fee(Amount::from_sat(fee)).unwrap();

        assert_eq!(updated_payout.maker_amount, Amount::from_sat(0));
        assert_eq!(
            updated_payout.taker_amount,
            Amount::from_sat(orig_taker_amount - (fee + orig_maker_amount))
        );
    }

    fn check_tx_fee(input_txs: &[&Transaction], spend_tx: &Transaction) -> Result<()> {
        let input_amount = spend_tx
            .input
            .iter()
            .try_fold::<_, _, Result<_>>(0, |acc, input| {
                let value = input_txs
                    .iter()
                    .find_map(|tx| {
                        (tx.txid() == input.previous_output.txid)
                            .then(|| tx.output[input.previous_output.vout as usize].value)
                    })
                    .with_context(|| {
                        format!(
                            "spend tx input {} not found in input_txs",
                            input.previous_output
                        )
                    })
                    .context("foo")?;

                Ok(acc + value)
            })?;

        let output_amount = spend_tx
            .output
            .iter()
            .fold(0, |acc, output| acc + output.value);
        let fee = input_amount - output_amount;

        let min_relay_fee = spend_tx.get_virtual_size();
        if (dbg!(fee) as f64) < dbg!(min_relay_fee) {
            bail!("min relay fee not met, {} < {}", fee, min_relay_fee)
        }

        Ok(())
    }

    fn build_wallet<R>(
        rng: &mut R,
        utxo_amount: Amount,
        num_utxos: u8,
    ) -> Result<bdk::Wallet<(), bdk::database::MemoryDatabase>>
    where
        R: RngCore + CryptoRng,
    {
        use bdk::{populate_test_db, testutils};

        let mut seed = [0u8; 32];
        rng.fill_bytes(&mut seed);

        let key = ExtendedPrivKey::new_master(Network::Regtest, &seed)?;
        let descriptors = testutils!(@descriptors (&format!("wpkh({}/*)", key)));

        let mut database = bdk::database::MemoryDatabase::new();

        for index in 0..num_utxos {
            populate_test_db!(
                &mut database,
                testutils! {
                    @tx ( (@external descriptors, index as u32) => utxo_amount.as_sat() ) (@confirmations 1)
                },
                Some(100)
            );
        }

        let wallet = bdk::Wallet::new_offline(&descriptors.0, None, Network::Regtest, database)?;

        Ok(wallet)
    }

    struct Oracle {
        key_pair: schnorrsig::KeyPair,
    }

    impl Oracle {
        fn new<R>(rng: &mut R) -> Self
        where
            R: RngCore + CryptoRng,
        {
            let key_pair = schnorrsig::KeyPair::new(SECP256K1, rng);

            Self { key_pair }
        }

        fn public_key(&self) -> schnorrsig::PublicKey {
            schnorrsig::PublicKey::from_keypair(SECP256K1, &self.key_pair)
        }

        fn attest(&self, event: &Event, msg: Message) -> schnorrsig::Signature {
            secp_utils::schnorr_sign_with_nonce(&msg.into(), &self.key_pair, &event.nonce)
        }
    }

    fn announce<R>(rng: &mut R) -> (Event, Announcement)
    where
        R: RngCore + CryptoRng,
    {
        let event = Event::new(rng);
        let announcement = event.announcement();

        (event, announcement)
    }

    /// Represents the oracle's commitment to a nonce that will be used to
    /// sign a specific event in the future.
    struct Event {
        /// Nonce.
        ///
        /// Must remain secret.
        nonce: SecretKey,
        nonce_pk: schnorrsig::PublicKey,
    }

    impl Event {
        fn new<R>(rng: &mut R) -> Self
        where
            R: RngCore + CryptoRng,
        {
            let nonce = SecretKey::new(rng);

            let key_pair = schnorrsig::KeyPair::from_secret_key(SECP256K1, nonce);
            let nonce_pk = schnorrsig::PublicKey::from_keypair(SECP256K1, &key_pair);

            Self { nonce, nonce_pk }
        }

        fn announcement(&self) -> Announcement {
            Announcement {
                nonce_pk: self.nonce_pk,
            }
        }
    }

    /// Public message which can be used by anyone to perform a DLC
    /// protocol based on a specific event.
    ///
    /// These would normally include more information to identify the
    /// specific event, but we omit this for simplicity. See:
    /// https://github.com/discreetlogcontracts/dlcspecs/blob/master/Oracle.md#oracle-events
    #[derive(Clone, Copy)]
    struct Announcement {
        nonce_pk: schnorrsig::PublicKey,
    }

    impl Announcement {
        fn nonce_pk(&self) -> schnorrsig::PublicKey {
            self.nonce_pk
        }
    }

    fn make_keypair<R>(rng: &mut R) -> (SecretKey, PublicKey)
    where
        R: RngCore + CryptoRng,
    {
        let sk = SecretKey::new(rng);
        let pk = PublicKey::from_private_key(
            SECP256K1,
            &PrivateKey {
                compressed: true,
                network: Network::Regtest,
                key: sk,
            },
        );

        (sk, pk)
    }

    /// Decompose a BIP340 signature into R and s.
    pub fn schnorrsig_decompose(
        signature: &schnorrsig::Signature,
    ) -> (schnorrsig::PublicKey, SecretKey) {
        let bytes = signature.as_ref();

        let nonce_pk = schnorrsig::PublicKey::from_slice(&bytes[0..32]).expect("R value in sig");
        let s = SecretKey::from_slice(&bytes[32..64]).expect("s value in sig");

        (nonce_pk, s)
    }

    mod secp_utils {
        use super::*;

        use secp256k1_zkp::secp256k1_zkp_sys::types::c_void;
        use secp256k1_zkp::secp256k1_zkp_sys::CPtr;
        use std::os::raw::{c_int, c_uchar};
        use std::ptr;

        /// Create a Schnorr signature using the provided nonce instead of generating one.
        pub fn schnorr_sign_with_nonce(
            msg: &secp256k1_zkp::Message,
            keypair: &schnorrsig::KeyPair,
            nonce: &SecretKey,
        ) -> schnorrsig::Signature {
            unsafe {
                let mut sig = [0u8; secp256k1_zkp::constants::SCHNORRSIG_SIGNATURE_SIZE];
                assert_eq!(
                    1,
                    secp256k1_zkp::ffi::secp256k1_schnorrsig_sign(
                        *SECP256K1.ctx(),
                        sig.as_mut_c_ptr(),
                        msg.as_c_ptr(),
                        keypair.as_ptr(),
                        Some(constant_nonce_fn),
                        nonce.as_c_ptr() as *const c_void
                    )
                );

                schnorrsig::Signature::from_slice(&sig).unwrap()
            }
        }

        extern "C" fn constant_nonce_fn(
            nonce32: *mut c_uchar,
            _msg32: *const c_uchar,
            _key32: *const c_uchar,
            _xonly_pk32: *const c_uchar,
            _algo16: *const c_uchar,
            data: *mut c_void,
        ) -> c_int {
            unsafe {
                ptr::copy_nonoverlapping(data as *const c_uchar, nonce32, 32);
            }
            1
        }
    }
}
