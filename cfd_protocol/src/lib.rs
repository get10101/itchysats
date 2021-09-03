use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use bdk::bitcoin::hashes::*;
use bdk::bitcoin::Txid;

use bdk::{
    bitcoin::{
        self, hashes::hex::ToHex, util::bip143::SigHashCache, util::psbt::Global,
        util::psbt::PartiallySignedTransaction, Address, Amount, Network, OutPoint, PublicKey,
        SigHash, SigHashType, Transaction, TxIn, TxOut,
    },
    descriptor::Descriptor,
    miniscript::{descriptor::Wsh, DescriptorTrait},
};
use itertools::Itertools;
use secp256k1_zkp::EcdsaAdaptorSignature;
use secp256k1_zkp::SecretKey;
use secp256k1_zkp::SECP256K1;
use secp256k1_zkp::{self, schnorrsig, Signature};
use std::collections::HashMap;

/// In satoshi per vbyte.
const MIN_RELAY_FEE: u64 = 1;

// TODO use Amount type
const P2PKH_DUST_LIMIT: u64 = 546;

pub trait WalletExt {
    fn build_lock_psbt(&self, amount: Amount, identity_pk: PublicKey) -> PartyParams;
}

impl<B, D> WalletExt for bdk::Wallet<B, D> {
    fn build_lock_psbt(&self, amount: Amount, identity_pk: PublicKey) -> PartyParams {
        todo!()
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

fn lock_descriptor(maker_pk: PublicKey, taker_pk: PublicKey) -> Descriptor<PublicKey> {
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
    // TODO: Optimize miniscript

    let maker_own_pk_hash = maker_own_pk.pubkey_hash().as_hash();
    let maker_own_pk = (&maker_own_pk.key.serialize().to_vec()).to_hex();
    let taker_own_pk_hash = taker_own_pk.pubkey_hash().as_hash();
    let taker_own_pk = (&taker_own_pk.key.serialize().to_vec()).to_hex();

    let maker_rev_pk_hash = maker_rev_pk.pubkey_hash().as_hash();
    let taker_rev_pk_hash = taker_rev_pk.pubkey_hash().as_hash();

    let maker_publish_pk_hash = maker_publish_pk.pubkey_hash().as_hash();
    let taker_publish_pk_hash = taker_publish_pk.pubkey_hash().as_hash();

    let cet_or_refund_condition = format!("and_v(v:pk({}),pk_k({}))", maker_own_pk, taker_own_pk);
    let maker_punish_condition = format!(
        "and_v(v:pkh({}),and_v(v:pkh({}),pk_h({})))",
        maker_own_pk_hash, taker_publish_pk_hash, taker_rev_pk_hash
    );
    let taker_punish_condition = format!(
        "and_v(v:pkh({}),and_v(v:pkh({}),pk_h({})))",
        taker_own_pk_hash, maker_publish_pk_hash, maker_rev_pk_hash
    );
    let descriptor_str = format!(
        "wsh(c:or_i(or_i({},{}),{}))",
        maker_punish_condition, taker_punish_condition, cet_or_refund_condition
    );

    descriptor_str.parse().expect("a valid miniscript")
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
    _tx: Transaction,
    _commit_descriptor: Descriptor<PublicKey>,
    (_maker_pk, _maker_sig): (PublicKey, Signature),
    (_taker_pk, _taker_sig): (PublicKey, Signature),
) -> Transaction {
    todo!()
}

// NOTE: We have decided to not offer any verification utility because
// the APIs would be incredibly thin

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
    fn to_txouts(&self, maker_address: &Address, taker_address: &Address) -> Vec<TxOut> {
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

#[derive(Debug, Clone, Copy, PartialEq)]
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
            output: payout.to_txouts(maker_address, taker_address),
        };

        let fee = tx.get_size() * MIN_RELAY_FEE as usize;
        let payout = payout.with_updated_fee(Amount::from_sat(fee as u64))?;
        tx.output = payout.to_txouts(maker_address, taker_address);

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

    fn encverify(
        &self,
        encsig: &EcdsaAdaptorSignature,
        signing_pk: &PublicKey,
        oracle_pk: &schnorrsig::PublicKey,
        nonce_pk: &schnorrsig::PublicKey,
    ) -> Result<()> {
        let signature_point = compute_signature_point(oracle_pk, nonce_pk, self.message)?;

        encsig.verify(
            SECP256K1,
            &secp256k1_zkp::Message::from_slice(&self.sighash).expect("sighash is valid message"),
            &signing_pk.key,
            &signature_point,
        )?;

        Ok(())
    }

    fn txid(&self) -> Txid {
        self.inner.txid()
    }

    pub fn add_signatures(
        self,
        (maker_pk, maker_sig): (PublicKey, Signature),
        (taker_pk, taker_sig): (PublicKey, Signature),
    ) -> Result<Transaction> {
        let satisfier = {
            let mut satisfier = HashMap::with_capacity(2);

            // The order in which these are inserted doesn't matter
            satisfier.insert(maker_pk, (maker_sig, SigHashType::All));
            satisfier.insert(taker_pk, (taker_sig, SigHashType::All));

            satisfier
        };

        let mut tx_refund = self.inner;
        self.commit_descriptor
            .satisfy(&mut tx_refund.input[0], satisfier)?;

        Ok(tx_refund)
    }
}

#[derive(Debug, Clone)]
struct PunishTransaction {
    inner: Transaction,
}

impl PunishTransaction {
    fn new(
        commit_tx: &CommitTransaction,
        address: &Address,
        encsig: EcdsaAdaptorSignature,
        sk: SecretKey,
        (revocation_them_sk, revocation_them_pk): (SecretKey, PublicKey), // FIXME: Only need sk
        publish_them_pk: PublicKey,
        revoked_commit_tx: &Transaction,
    ) -> Result<Self> {
        // CommitTransaction has only one input
        let input = revoked_commit_tx.input.clone().into_iter().exactly_one()?;

        // Extract all signatures from witness stack
        let mut sigs = Vec::new();
        for witness in input.witness.iter() {
            let witness = witness.as_slice();

            let res = bitcoin::secp256k1::Signature::from_der(&witness[..witness.len() - 1]);
            match res {
                Ok(sig) => sigs.push(sig),
                Err(_) => {
                    continue;
                }
            }
        }

        if sigs.is_empty() {
            // No signature found, this should fail
            unimplemented!()
        }

        // Attempt to extract y_other from every signature
        let publish_them_sk = sigs
            .into_iter()
            .find_map(|sig| encsig.recover(SECP256K1, &sig, &publish_them_pk.key).ok())
            .context("Could not recover secret key from revoked transaction")?;

        // Fixme: need to subtract tx fee otherwise we won't be able to publish this transaction.
        let mut punish_tx = {
            let output = TxOut {
                value: commit_tx.amount().as_sat(),
                script_pubkey: address.script_pubkey(),
            };
            Transaction {
                version: 2,
                lock_time: 0,
                input: vec![TxIn {
                    previous_output: commit_tx.outpoint(),
                    ..Default::default()
                }],
                output: vec![output],
            }
        };

        let digest = Self::compute_digest(&punish_tx, commit_tx);

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

        commit_tx
            .descriptor()
            .satisfy(&mut punish_tx.input[0], satisfier)?;

        Ok(Self { inner: punish_tx })
    }

    fn compute_digest(punish_tx: &Transaction, commit_tx: &CommitTransaction) -> SigHash {
        SigHashCache::new(punish_tx).signature_hash(
            0,
            &commit_tx.descriptor().script_code(),
            commit_tx.amount().as_sat(),
            SigHashType::All,
        )
    }
}

#[derive(Debug, Clone)]
struct RefundTransaction {
    inner: Transaction,
    sighash: SigHash,
    commit_output_descriptor: Descriptor<PublicKey>,
}

impl RefundTransaction {
    /// Refund transaction fee. It is paid evenly by the maker and the
    /// taker.
    ///
    /// Ideally we don't commit to a transaction fee ahead of time and
    /// instead resort to fee-bumping. But that would be unfair for the
    /// party that executes the fee-bumping.
    ///
    /// TODO: Calculate reasonable fee given the fact that the
    /// transaction consists of 1 input and 2 outputs.
    const REFUND_TX_FEE: u64 = 10_000;

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

        let per_party_fee = Self::REFUND_TX_FEE / 2;

        let maker_output = TxOut {
            value: maker_amount.as_sat() - per_party_fee,
            script_pubkey: maker_address.script_pubkey(),
        };

        let taker_output = TxOut {
            value: taker_amount.as_sat() - per_party_fee,
            script_pubkey: taker_address.script_pubkey(),
        };

        let tx = Transaction {
            version: 2,
            lock_time: 0,
            input: vec![commit_input],
            output: vec![maker_output, taker_output],
        };

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

    pub fn add_signatures(
        self,
        (maker_pk, maker_sig): (PublicKey, Signature),
        (taker_pk, taker_sig): (PublicKey, Signature),
    ) -> Result<Transaction> {
        let satisfier = {
            let mut satisfier = HashMap::with_capacity(2);

            // The order in which these are inserted doesn't matter
            satisfier.insert(maker_pk, (maker_sig, SigHashType::All));
            satisfier.insert(taker_pk, (taker_sig, SigHashType::All));

            satisfier
        };

        let mut tx_refund = self.inner;
        self.commit_output_descriptor
            .satisfy(&mut tx_refund.input[0], satisfier)?;

        Ok(tx_refund)
    }
}

#[derive(Debug, Clone)]
struct CommitTransaction {
    inner: Transaction,
    descriptor: Descriptor<PublicKey>,
    amount: Amount,
    sighash: SigHash,
    lock_descriptor: Descriptor<PublicKey>,
}

impl CommitTransaction {
    fn new(
        lock_tx: &LockTransaction,
        (maker_own_pk, maker_rev_pk, maker_publish_pk): (PublicKey, PublicKey, PublicKey),
        (taker_own_pk, taker_rev_pk, taker_publish_pk): (PublicKey, PublicKey, PublicKey),
    ) -> Result<Self> {
        // FIXME: Fee to be paid by leftover lock output
        let amount = lock_tx.amount();

        let lock_input = TxIn {
            previous_output: lock_tx.lock_outpoint(),
            ..Default::default()
        };

        let descriptor = commit_descriptor(
            (maker_own_pk, maker_rev_pk, maker_publish_pk),
            (taker_own_pk, taker_rev_pk, taker_publish_pk),
        );

        let output = TxOut {
            value: lock_tx.amount().as_sat(),
            script_pubkey: descriptor
                .address(Network::Regtest)
                .expect("can derive address from descriptor")
                .script_pubkey(),
        };

        let inner = Transaction {
            version: 2,
            lock_time: 0,
            input: vec![lock_input],
            output: vec![output],
        };

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
            amount,
            sighash,
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

    fn encverify(
        &self,
        encsig: &EcdsaAdaptorSignature,
        signing_pk: &PublicKey,
        encryption_pk: &PublicKey,
    ) -> Result<()> {
        encsig.verify(
            SECP256K1,
            &secp256k1_zkp::Message::from_slice(&self.sighash).expect("sighash is valid message"),
            &signing_pk.key,
            &encryption_pk.key,
        )?;

        Ok(())
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

    fn sighash(&self) -> SigHash {
        self.sighash
    }

    pub fn add_signatures(
        self,
        (maker_pk, maker_sig): (PublicKey, Signature),
        (taker_pk, taker_sig): (PublicKey, Signature),
    ) -> Result<Transaction> {
        let satisfier = {
            let mut satisfier = HashMap::with_capacity(2);

            // The order in which these are inserted doesn't matter
            satisfier.insert(maker_pk, (maker_sig, SigHashType::All));
            satisfier.insert(taker_pk, (taker_sig, SigHashType::All));

            satisfier
        };

        let mut tx_commit = self.inner;
        self.lock_descriptor
            .satisfy(&mut tx_commit.input[0], satisfier)?;

        Ok(tx_commit)
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

    fn to_psbt(&self) -> PartiallySignedTransaction {
        self.inner.clone()
    }

    fn descriptor(&self) -> Descriptor<PublicKey> {
        self.lock_descriptor.clone()
    }

    fn amount(&self) -> Amount {
        self.amount
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdk::bitcoin::{util::bip32::ExtendedPrivKey, PrivateKey};
    use bdk::wallet::AddressIndex;
    use bdk::SignOptions;
    use bitcoin::Script;
    use rand::SeedableRng;
    use rand::{CryptoRng, RngCore};
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
            Payout {
                message: Message::Win,
                maker_amount: Amount::from_btc(2.0).unwrap(),
                taker_amount: Amount::ZERO,
            },
            Payout {
                message: Message::Lose,
                maker_amount: Amount::ZERO,
                taker_amount: Amount::from_btc(2.0).unwrap(),
            },
        ];

        // FIXME: Choose a value based on the network time or current
        // block height
        let refund_timelock = 0;

        let maker_wallet = build_wallet(&mut rng, Amount::from_btc(0.4).unwrap(), 5).unwrap();
        let taker_wallet = build_wallet(&mut rng, Amount::from_btc(0.4).unwrap(), 5).unwrap();

        let maker_address = maker_wallet.get_address(AddressIndex::New).unwrap();
        let taker_address = taker_wallet.get_address(AddressIndex::New).unwrap();

        // NOTE: We are probably paying too many transaction fees
        let (maker_psbt, _) = {
            let mut builder = maker_wallet.build_tx();
            builder
                .ordering(bdk::wallet::tx_builder::TxOrdering::Bip69Lexicographic)
                .add_recipient(Script::new(), maker_lock_amount.as_sat());
            builder.finish().unwrap()
        };

        let (taker_psbt, _) = {
            let mut builder = taker_wallet.build_tx();
            builder
                .ordering(bdk::wallet::tx_builder::TxOrdering::Bip69Lexicographic)
                .add_recipient(Script::new(), taker_lock_amount.as_sat());
            builder.finish().unwrap()
        };

        let lock_amount = maker_lock_amount + taker_lock_amount;
        let (maker_revocation_sk, maker_revocation_pk) = make_keypair(&mut rng);
        let (maker_publish_sk, maker_publish_pk) = make_keypair(&mut rng);

        let (taker_revocation_sk, taker_revocation_pk) = make_keypair(&mut rng);
        let (taker_publish_sk, taker_publish_pk) = make_keypair(&mut rng);

        let maker_cfd_txs = build_cfd_transactions(
            (
                PartyParams {
                    lock_psbt: maker_psbt.clone(),
                    identity_pk: maker_pk,
                    lock_amount: maker_lock_amount,
                    address: maker_address.address.clone(),
                },
                PunishParams {
                    revocation_pk: maker_revocation_pk,
                    publish_pk: maker_publish_pk,
                },
            ),
            (
                PartyParams {
                    lock_psbt: taker_psbt.clone(),
                    identity_pk: taker_pk,
                    lock_amount: taker_lock_amount,
                    address: taker_address.address.clone(),
                },
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
                PartyParams {
                    lock_psbt: maker_psbt,
                    identity_pk: maker_pk,
                    lock_amount: maker_lock_amount,
                    address: maker_address.address,
                },
                PunishParams {
                    revocation_pk: maker_revocation_pk,
                    publish_pk: maker_publish_pk,
                },
            ),
            (
                PartyParams {
                    lock_psbt: taker_psbt,
                    identity_pk: taker_pk,
                    lock_amount: taker_lock_amount,
                    address: taker_address.address,
                },
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
                    &event.announcement().nonce_pk(),
                    maker_cet.2,
                )
                .unwrap();
                let taker_encryption_point = compute_signature_point(
                    &oracle.public_key(),
                    &event.announcement().nonce_pk(),
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

        // let signed_commit_tx = commit_tx
        //     .clone()
        //     .add_signatures((maker_pk, maker_commit_sig), (taker_pk, taker_commit_sig))
        //     .expect("To be signed");

        // // verify refund transaction

        // lock_tx
        //     .descriptor()
        //     .address(Network::Regtest)
        //     .expect("can derive address from descriptor")
        //     .script_pubkey()
        //     .verify(
        //         0,
        //         lock_tx.amount().as_sat(),
        //         bitcoin::consensus::serialize(&signed_commit_tx).as_slice(),
        //     )
        //     .expect("valid signed commit transaction");

        // let signed_refund_tx = refund_tx
        //     .add_signatures((maker_pk, maker_refund_sig), (taker_pk, taker_refund_sig))
        //     .unwrap();

        // let commit_tx_amount = commit_tx.amount().as_sat();
        // commit_tx
        //     .descriptor()
        //     .address(Network::Regtest)
        //     .expect("can derive address from descriptor")
        //     .script_pubkey()
        //     .verify(
        //         0,
        //         commit_tx_amount,
        //         bitcoin::consensus::serialize(&signed_refund_tx).as_slice(),
        //     )
        //     .expect("valid signed refund transaction");

        // // verify cets

        // let attestations = [Message::Win, Message::Lose]
        //     .iter()
        //     .map(|msg| (*msg, oracle.attest(&event, *msg)))
        //     .collect::<Vec<_>>();

        // // TODO: Rewrite this so that it doesn't rely on the order of
        // // the two zipped iterators
        // let cets_with_sigs: Vec<(
        //     ContractExecutionTransaction,
        //     EcdsaAdaptorSignature,
        //     EcdsaAdaptorSignature,
        // )> = cets_with_maker_encsig
        //     .into_iter()
        //     .zip(cets_with_taker_encsig)
        //     .map(|((cet, maker_encsig), (_, taker_encsig))| (cet, maker_encsig, taker_encsig))
        //     .collect::<Vec<_>>();

        // let signed_cets = attestations
        //     .iter()
        //     .map(|(oracle_msg, oracle_sig)| {
        //         let (cet, maker_encsig, taker_encsig) = cets_with_sigs
        //             .iter()
        //             .find(|(cet, _, _)| &cet.message == oracle_msg)
        //             .expect("one cet per message");
        //         let (_nonce_pk, signature_scalar) = schnorrsig_decompose(oracle_sig).unwrap();

        //         let maker_sig = maker_encsig.decrypt(&signature_scalar)?;
        //         let taker_sig = taker_encsig.decrypt(&signature_scalar)?;

        //         cet.clone()
        //             .add_signatures((maker_pk, maker_sig), (taker_pk, taker_sig))
        //     })
        //     .collect::<Result<Vec<_>>>()
        //     .unwrap();

        // signed_cets
        //     .iter()
        //     .try_for_each(|cet| {
        //         commit_tx
        //             .descriptor()
        //             .address(Network::Regtest)
        //             .expect("can derive address from descriptor")
        //             .script_pubkey()
        //             .verify(
        //                 0,
        //                 commit_tx_amount,
        //                 bitcoin::consensus::serialize(&cet).as_slice(),
        //             )
        //     })
        //     .expect("valid signed CETs");

        // // verify punishment transactions

        // let punish_tx = PunishTransaction::new(
        //     &commit_tx,
        //     &maker_address,
        //     maker_commit_encsig,
        //     maker_sk,
        //     (taker_revocation_sk, taker_revocation_pk),
        //     taker_publish_pk,
        //     &signed_commit_tx,
        // )
        // .unwrap();

        // commit_tx
        //     .descriptor()
        //     .address(Network::Regtest)
        //     .expect("can derive address from descriptor")
        //     .script_pubkey()
        //     .verify(
        //         0,
        //         commit_tx_amount,
        //         bitcoin::consensus::serialize(&punish_tx.inner).as_slice(),
        //     )
        //     .expect("valid signed punish transaction");

        // let punish_tx = PunishTransaction::new(
        //     &commit_tx,
        //     &taker_address,
        //     taker_commit_encsig,
        //     taker_sk,
        //     (maker_revocation_sk, maker_revocation_pk),
        //     maker_publish_pk,
        //     &signed_commit_tx,
        // )
        // .unwrap();

        // commit_tx
        //     .descriptor()
        //     .address(Network::Regtest)
        //     .expect("can derive address from descriptor")
        //     .script_pubkey()
        //     .verify(
        //         0,
        //         commit_tx_amount,
        //         bitcoin::consensus::serialize(&punish_tx.inner).as_slice(),
        //     )
        //     .expect("valid signed punish transaction");
    }

    #[test]
    fn test_fee_subtraction_bigger_than_dust() {
        let orig_maker_amount = 1000;
        let orig_taker_amount = 1000;
        let payout = Payout {
            message: Message::Win,
            maker_amount: Amount::from_sat(orig_maker_amount),
            taker_amount: Amount::from_sat(orig_taker_amount),
        };
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
        let payout = Payout {
            message: Message::Win,
            maker_amount: Amount::from_sat(orig_maker_amount),
            taker_amount: Amount::from_sat(orig_taker_amount),
        };
        let fee = 100;
        let updated_payout = payout.with_updated_fee(Amount::from_sat(fee)).unwrap();

        assert_eq!(updated_payout.maker_amount, Amount::from_sat(0));
        assert_eq!(
            updated_payout.taker_amount,
            Amount::from_sat(orig_taker_amount - (fee + orig_maker_amount))
        );
    }

    fn build_wallet<R>(
        rng: &mut R,
        utxo_amount: Amount,
        num_utxos: u8,
    ) -> Result<bdk::Wallet<(), bdk::database::MemoryDatabase>>
    where
        R: RngCore + CryptoRng,
    {
        use bdk::populate_test_db;
        use bdk::testutils;

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
    ) -> Result<(schnorrsig::PublicKey, SecretKey)> {
        let bytes = signature.as_ref();

        let nonce_pk = schnorrsig::PublicKey::from_slice(&bytes[0..32])?;
        let s = SecretKey::from_slice(&bytes[32..64])?;

        Ok((nonce_pk, s))
    }

    mod secp_utils {
        use super::*;

        use secp256k1_zkp::secp256k1_zkp_sys::types::c_void;
        use secp256k1_zkp::secp256k1_zkp_sys::CPtr;
        use std::os::raw::c_int;
        use std::os::raw::c_uchar;
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
