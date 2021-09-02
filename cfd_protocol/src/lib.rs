use anyhow::Result;
use bdk::bitcoin::hashes::*;
use bdk::bitcoin::Txid;
use bdk::SignOptions;
use bdk::{
    bitcoin::{
        self,
        hashes::hex::ToHex,
        util::bip143::SigHashCache,
        util::psbt::Global,
        util::{bip32::ExtendedPrivKey, psbt::PartiallySignedTransaction},
        Address, Amount, Network, OutPoint, PrivateKey, PublicKey, Script, SigHash, SigHashType,
        Transaction, TxIn, TxOut,
    },
    descriptor::Descriptor,
    miniscript::{descriptor::Wsh, DescriptorTrait},
    wallet::AddressIndex,
};
use rand::RngCore;
use rand::{CryptoRng, SeedableRng};
use rand_chacha::ChaChaRng;
use secp256k1_zkp::EcdsaAdaptorSignature;
use secp256k1_zkp::SECP256K1;
use secp256k1_zkp::{self, schnorrsig, Signature};
use secp256k1_zkp::{Secp256k1, SecretKey};
use std::collections::HashMap;

// TODO: Consider extension trait
pub fn build_lock_psbt<B, D>(
    wallet: &bdk::Wallet<B, D>,
    caller_dlc_amount: Amount,
) -> Result<PartiallySignedTransaction> {
    todo!()
}

// TODO: Consider how this might change with off-chain protocol
pub fn build_dlc_transactions(
    maker: Party,
    taker: Party,
    refund_timelock: u32,
    payouts: Vec<Payout>,
    signing_sk: SecretKey,
) -> Result<DlcTransactions> {
    todo!()
}

fn dlc_descriptor(maker_pk: PublicKey, taker_pk: PublicKey) -> Descriptor<PublicKey> {
    todo!()
}

fn spend_tx_sighash(
    tx: &Transaction,
    dlc_descriptor: &Descriptor<PublicKey>,
    dlc_amount: Amount,
) -> SigHash {
    todo!()
}

pub fn finalize_spend_transaction(
    tx: Transaction,
    dlc_descriptor: Descriptor<PublicKey>,
    (maker_pk, maker_sig): (PublicKey, Signature),
    (taker_pk, taker_sig): (PublicKey, Signature),
) -> Transaction {
    todo!()
}

// NOTE: We have decided to not offer any verification utility because
// the APIs would be incredibly thin

pub struct Party {
    pub lock_psbt: PartiallySignedTransaction,
    pub dlc_pk: PublicKey,
    pub dlc_amount: Amount,
    pub address: Address,
}

pub struct OracleParams {
    pub pk: schnorrsig::PublicKey,
    pub nonce_pk: schnorrsig::PublicKey,
}

pub struct DlcTransactions {
    pub lock_psbt: PartiallySignedTransaction,
    pub cets: Vec<(Transaction, EcdsaAdaptorSignature)>,
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
    fn to_txouts(self, maker_address: &Address, taker_address: &Address, fee: u64) -> Vec<TxOut> {
        let mut txouts = [
            (self.maker_amount, maker_address),
            (self.taker_amount, taker_address),
        ]
        .iter()
        .filter_map(|(amount, address)| {
            (amount != &Amount::ZERO).then(|| TxOut {
                value: amount.as_sat(),
                script_pubkey: address.script_pubkey(),
            })
        })
        .collect::<Vec<_>>();

        // TODO: Rewrite this
        if txouts.len() == 1 {
            txouts[0].value -= fee;
        } else if txouts.len() == 2 {
            txouts[0].value -= fee / 2;
            txouts[1].value -= fee / 2;
        }

        txouts
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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

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

    /// In satoshi per vbyte.
    const MIN_RELAY_FEE: u64 = 1;

    #[test]
    fn run_cfd_protocol() {
        let mut rng = ChaChaRng::seed_from_u64(0);
        let secp = Secp256k1::new();

        let maker_dlc_amount = Amount::ONE_BTC;
        let taker_dlc_amount = Amount::ONE_BTC;

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
                .add_recipient(Script::new(), maker_dlc_amount.as_sat());
            builder.finish().unwrap()
        };

        let (taker_psbt, _) = {
            let mut builder = taker_wallet.build_tx();
            builder
                .ordering(bdk::wallet::tx_builder::TxOrdering::Bip69Lexicographic)
                .add_recipient(Script::new(), taker_dlc_amount.as_sat());
            builder.finish().unwrap()
        };

        let dlc_amount = maker_dlc_amount + taker_dlc_amount;
        let lock_tx =
            LockTransaction::new(maker_psbt, taker_psbt, maker_pk, taker_pk, dlc_amount).unwrap();

        let (maker_revocation_sk, maker_revocation_pk) = make_keypair(&mut rng);
        let (maker_publish_sk, maker_publish_pk) = make_keypair(&mut rng);

        let (taker_revocation_sk, taker_revocation_pk) = make_keypair(&mut rng);
        let (taker_publish_sk, taker_publish_pk) = make_keypair(&mut rng);

        let commit_tx = CommitTransaction::new(
            &lock_tx,
            (maker_pk, maker_revocation_pk, maker_publish_pk),
            (taker_pk, taker_revocation_pk, taker_publish_pk),
        )
        .unwrap();

        // Construct refund TX
        let refund_tx = RefundTransaction::new(
            &commit_tx,
            refund_timelock,
            &maker_address,
            &taker_address,
            maker_dlc_amount,
            taker_dlc_amount,
        );

        // Construct CET TXs
        let cets = payouts
            .iter()
            .map(|payout| {
                ContractExecutionTransaction::new(
                    &commit_tx,
                    payout,
                    &maker_address,
                    &taker_address,
                    12,
                )
            })
            .collect::<Vec<_>>();

        let maker_refund_sig = {
            let sighash = secp256k1_zkp::Message::from_slice(&refund_tx.sighash()).unwrap();
            let sig = secp.sign(&sighash, &maker_sk);

            secp.verify(&sighash, &sig, &maker_pk.key)
                .expect("valid maker refund sig");
            sig
        };

        let taker_refund_sig = {
            let sighash = secp256k1_zkp::Message::from_slice(&refund_tx.sighash()).unwrap();
            let sig = secp.sign(&sighash, &taker_sk);

            secp.verify(&sighash, &sig, &taker_pk.key)
                .expect("valid taker refund sig");
            sig
        };

        let maker_cet_encsigs = cets
            .iter()
            .map(|cet| {
                (
                    cet.txid(),
                    cet.encsign(maker_sk, &oracle.public_key(), &announcement.nonce_pk())
                        .unwrap(),
                )
            })
            .collect::<Vec<_>>();

        let cets_with_maker_encsig = cets
            .clone()
            .into_iter()
            .map(|cet| {
                let encsig = maker_cet_encsigs
                    .iter()
                    .find_map(|(txid, encsig)| (txid == &cet.txid()).then(|| encsig))
                    .expect("one maker encsig per cet");

                cet.encverify(
                    encsig,
                    &maker_pk,
                    &oracle.public_key(),
                    &announcement.nonce_pk(),
                )?;

                Ok((cet, *encsig))
            })
            .collect::<Result<Vec<_>>>()
            .expect("valid maker cet encsigs");

        let taker_cet_encsigs = cets
            .iter()
            .map(|cet| {
                (
                    cet.txid(),
                    cet.encsign(taker_sk, &oracle.public_key(), &announcement.nonce_pk())
                        .unwrap(),
                )
            })
            .collect::<Vec<_>>();

        let cets_with_taker_encsig = cets
            .iter()
            .map(|cet| {
                let encsig = taker_cet_encsigs
                    .iter()
                    .find_map(|(txid, encsig)| (txid == &cet.txid()).then(|| encsig))
                    .expect("one taker encsig per cet");

                cet.encverify(
                    encsig,
                    &taker_pk,
                    &oracle.public_key(),
                    &announcement.nonce_pk(),
                )?;

                Ok((cet, *encsig))
            })
            .collect::<Result<Vec<_>>>()
            .expect("valid taker cet encsigs");

        // encsign commit transaction

        let (maker_commit_encsig, maker_commit_sig) = {
            let encsig = commit_tx.encsign(maker_sk, &taker_publish_pk).unwrap();

            commit_tx
                .encverify(&encsig, &maker_pk, &taker_publish_pk)
                .expect("valid maker commit encsig");

            let sig = encsig.decrypt(&taker_publish_sk).unwrap();

            (encsig, sig)
        };

        let (taker_commit_encsig, taker_commit_sig) = {
            let encsig = commit_tx.encsign(taker_sk, &maker_publish_pk).unwrap();

            commit_tx
                .encverify(&encsig, &taker_pk, &maker_publish_pk)
                .expect("valid taker commit encsig");

            let sig = encsig.decrypt(&maker_publish_sk).unwrap();

            (encsig, sig)
        };

        // sign lock transaction

        let mut signed_lock_tx = lock_tx.to_psbt();
        maker_wallet
            .sign(
                &mut signed_lock_tx,
                SignOptions {
                    trust_witness_utxo: true,
                    ..Default::default()
                },
            )
            .unwrap();

        maker_wallet
            .sign(
                &mut signed_lock_tx,
                SignOptions {
                    trust_witness_utxo: true,
                    ..Default::default()
                },
            )
            .unwrap();

        let signed_commit_tx = commit_tx
            .clone()
            .add_signatures((maker_pk, maker_commit_sig), (taker_pk, taker_commit_sig))
            .expect("To be signed");

        // verify refund transaction

        lock_tx
            .descriptor()
            .address(Network::Regtest)
            .expect("can derive address from descriptor")
            .script_pubkey()
            .verify(
                0,
                lock_tx.amount().as_sat(),
                bitcoin::consensus::serialize(&signed_commit_tx).as_slice(),
            )
            .expect("valid signed commit transaction");

        let signed_refund_tx = refund_tx
            .add_signatures((maker_pk, maker_refund_sig), (taker_pk, taker_refund_sig))
            .unwrap();

        let commit_tx_amount = commit_tx.amount().as_sat();
        commit_tx
            .descriptor()
            .address(Network::Regtest)
            .expect("can derive address from descriptor")
            .script_pubkey()
            .verify(
                0,
                commit_tx_amount,
                bitcoin::consensus::serialize(&signed_refund_tx).as_slice(),
            )
            .expect("valid signed refund transaction");

        // verify cets

        let attestations = [Message::Win, Message::Lose]
            .iter()
            .map(|msg| (*msg, oracle.attest(&event, *msg)))
            .collect::<Vec<_>>();

        // TODO: Rewrite this so that it doesn't rely on the order of
        // the two zipped iterators
        let cets_with_sigs: Vec<(
            ContractExecutionTransaction,
            EcdsaAdaptorSignature,
            EcdsaAdaptorSignature,
        )> = cets_with_maker_encsig
            .into_iter()
            .zip(cets_with_taker_encsig)
            .map(|((cet, maker_encsig), (_, taker_encsig))| (cet, maker_encsig, taker_encsig))
            .collect::<Vec<_>>();

        let signed_cets = attestations
            .iter()
            .map(|(oracle_msg, oracle_sig)| {
                let (cet, maker_encsig, taker_encsig) = cets_with_sigs
                    .iter()
                    .find(|(cet, _, _)| &cet.message == oracle_msg)
                    .expect("one cet per message");
                let (_nonce_pk, signature_scalar) = schnorrsig_decompose(oracle_sig).unwrap();

                let maker_sig = maker_encsig.decrypt(&signature_scalar)?;
                let taker_sig = taker_encsig.decrypt(&signature_scalar)?;

                cet.clone()
                    .add_signatures((maker_pk, maker_sig), (taker_pk, taker_sig))
            })
            .collect::<Result<Vec<_>>>()
            .unwrap();

        signed_cets
            .iter()
            .try_for_each(|cet| {
                commit_tx
                    .descriptor()
                    .address(Network::Regtest)
                    .expect("can derive address from descriptor")
                    .script_pubkey()
                    .verify(
                        0,
                        commit_tx_amount,
                        bitcoin::consensus::serialize(&cet).as_slice(),
                    )
            })
            .expect("valid signed CETs");

        // verify punishment transactions

        let punish_tx = PunishTransaction::new(
            &commit_tx,
            &maker_address,
            maker_commit_encsig,
            maker_sk,
            (taker_revocation_sk, taker_revocation_pk),
            taker_publish_pk,
            &signed_commit_tx,
        )
        .unwrap();

        commit_tx
            .descriptor()
            .address(Network::Regtest)
            .expect("can derive address from descriptor")
            .script_pubkey()
            .verify(
                0,
                commit_tx_amount,
                bitcoin::consensus::serialize(&punish_tx.inner).as_slice(),
            )
            .expect("valid signed punish transaction");

        let punish_tx = PunishTransaction::new(
            &commit_tx,
            &taker_address,
            taker_commit_encsig,
            taker_sk,
            (maker_revocation_sk, maker_revocation_pk),
            maker_publish_pk,
            &signed_commit_tx,
        )
        .unwrap();

        commit_tx
            .descriptor()
            .address(Network::Regtest)
            .expect("can derive address from descriptor")
            .script_pubkey()
            .verify(
                0,
                commit_tx_amount,
                bitcoin::consensus::serialize(&punish_tx.inner).as_slice(),
            )
            .expect("valid signed punish transaction");
    }

    const BIP340_MIDSTATE: [u8; 32] = [
        0x9c, 0xec, 0xba, 0x11, 0x23, 0x92, 0x53, 0x81, 0x11, 0x67, 0x91, 0x12, 0xd1, 0x62, 0x7e,
        0x0f, 0x97, 0xc8, 0x75, 0x50, 0x00, 0x3c, 0xc7, 0x65, 0x90, 0xf6, 0x11, 0x64, 0x33, 0xe9,
        0xb6, 0x6a,
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
        fn schnorr_pubkey_to_pubkey(
            pk: &schnorrsig::PublicKey,
        ) -> Result<secp256k1_zkp::PublicKey> {
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
        ) -> Self {
            let commit_input = TxIn {
                previous_output: commit_tx.outpoint(),
                sequence: relative_timelock_in_blocks,
                ..Default::default()
            };

            let fee = Self::calculate_vbytes() * MIN_RELAY_FEE;

            let tx = Transaction {
                version: 2,
                lock_time: 0,
                input: vec![commit_input],
                output: payout.to_txouts(maker_address, taker_address, fee),
            };

            let sighash = SigHashCache::new(&tx).signature_hash(
                0,
                &commit_tx.descriptor.script_code(),
                commit_tx.amount.as_sat(),
                SigHashType::All,
            );

            Self {
                inner: tx,
                message: payout.message,
                sighash,
                commit_descriptor: commit_tx.descriptor(),
            }
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
                &secp256k1_zkp::Message::from_slice(&self.sighash)
                    .expect("sighash is valid message"),
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
                &secp256k1_zkp::Message::from_slice(&self.sighash)
                    .expect("sighash is valid message"),
                &signing_pk.key,
                &signature_point,
            )?;

            Ok(())
        }

        fn txid(&self) -> Txid {
            self.inner.txid()
        }

        fn calculate_vbytes() -> u64 {
            // TODO: Do it properly
            100
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
            let input = revoked_commit_tx.input[0].clone();

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
                .find_map(|sig| encsig.recover(&SECP256K1, &sig, &publish_them_pk.key).ok())
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

            let digest = Self::compute_digest(&punish_tx, &commit_tx);

            let satisfier = {
                let mut satisfier = HashMap::with_capacity(3);

                let pk = bitcoin::secp256k1::PublicKey::from_secret_key(SECP256K1, &sk);
                let pk_hash = hash160::Hash::hash(&pk.serialize()[..]);
                let pk = bitcoin::PublicKey {
                    compressed: true,
                    key: pk,
                };
                let sig_sk = SECP256K1.sign(&secp256k1_zkp::Message::from_slice(&digest)?, &sk);

                let publish_them_pk_hash =
                    hash160::Hash::hash(&publish_them_pk.key.serialize()[..]);
                let sig_publish_other = SECP256K1.sign(
                    &secp256k1_zkp::Message::from_slice(&digest)?,
                    &publish_them_sk,
                );

                let revocation_them_pk_hash =
                    hash160::Hash::hash(&revocation_them_pk.key.serialize()[..]);
                let sig_revocation_other = SECP256K1.sign(
                    &secp256k1_zkp::Message::from_slice(&digest)?,
                    &revocation_them_sk,
                );

                satisfier.insert(pk_hash, (pk, (sig_sk.into(), SigHashType::All)));

                satisfier.insert(
                    publish_them_pk_hash,
                    (
                        publish_them_pk,
                        (sig_publish_other.into(), SigHashType::All),
                    ),
                );
                satisfier.insert(
                    revocation_them_pk_hash,
                    (
                        revocation_them_pk,
                        (sig_revocation_other.into(), SigHashType::All),
                    ),
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
        fn new(
            commit_tx: &CommitTransaction,
            relative_locktime_in_blocks: u32,
            maker_address: &Address,
            taker_address: &Address,
            maker_amount: Amount,
            taker_amount: Amount,
        ) -> Self {
            let dlc_input = TxIn {
                previous_output: commit_tx.outpoint(),
                sequence: relative_locktime_in_blocks,
                ..Default::default()
            };

            let per_party_fee = REFUND_TX_FEE / 2;

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
                input: vec![dlc_input],
                output: vec![maker_output, taker_output],
            };

            let commit_output_descriptor = commit_tx.descriptor().clone();

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

            let descriptor = Self::build_descriptor(
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

        fn encsign(
            &self,
            sk: SecretKey,
            publish_them_pk: &PublicKey,
        ) -> Result<EcdsaAdaptorSignature> {
            Ok(EcdsaAdaptorSignature::encrypt(
                SECP256K1,
                &secp256k1_zkp::Message::from_slice(&self.sighash)
                    .expect("sighash is valid message"),
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
                &secp256k1_zkp::Message::from_slice(&self.sighash)
                    .expect("sighash is valid message"),
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
                .expect("to find dlc output in lock tx");

            OutPoint {
                txid,
                vout: vout as u32,
            }
        }

        fn build_descriptor(
            (maker_own_pk, maker_rev_pk, maker_publish_pk): (PublicKey, PublicKey, PublicKey),
            (taker_own_pk, taker_rev_pk, taker_publish_pk): (PublicKey, PublicKey, PublicKey),
        ) -> Descriptor<PublicKey> {
            // TODO: Optimize miniscript

            let maker_own_pk_hash = hash160::Hash::hash(&maker_own_pk.key.serialize()[..]);
            let maker_own_pk = (&maker_own_pk.key.serialize().to_vec()).to_hex();
            let taker_own_pk_hash = hash160::Hash::hash(&taker_own_pk.key.serialize()[..]);
            let taker_own_pk = (&taker_own_pk.key.serialize().to_vec()).to_hex();

            let maker_rev_pk_hash = hash160::Hash::hash(&maker_rev_pk.key.serialize()[..]);
            let taker_rev_pk_hash = hash160::Hash::hash(&taker_rev_pk.key.serialize()[..]);

            let maker_publish_pk_hash = hash160::Hash::hash(&maker_publish_pk.key.serialize()[..]);
            let taker_publish_pk_hash = hash160::Hash::hash(&taker_publish_pk.key.serialize()[..]);

            let cet_or_refund_condition =
                format!("and_v(v:pk({}),pk_k({}))", maker_own_pk, taker_own_pk);
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
            let lock_descriptor = Self::build_descriptor(maker_pk, taker_pk);

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
                .expect("to find dlc output in lock tx");

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

        fn build_descriptor(maker_pk: PublicKey, taker_pk: PublicKey) -> Descriptor<PublicKey> {
            const MINISCRIPT_TEMPLATE: &str = "c:and_v(v:pk(A),pk_k(B))";

            let maker_pk = ToHex::to_hex(&maker_pk.key);
            let taker_pk = ToHex::to_hex(&taker_pk.key);

            let miniscript = MINISCRIPT_TEMPLATE
                .replace("A", &maker_pk)
                .replace("B", &taker_pk);

            let miniscript = miniscript.parse().expect("a valid miniscript");

            Descriptor::Wsh(Wsh::new(miniscript).expect("a valid descriptor"))
        }

        fn amount(&self) -> Amount {
            self.amount
        }
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
