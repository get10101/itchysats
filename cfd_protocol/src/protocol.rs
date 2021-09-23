pub use transaction_ext::TransactionExt;
pub use transactions::{close_transaction, punish_transaction};

use crate::interval;
use crate::protocol::sighash_ext::SigHashExt;
use crate::protocol::transactions::{
    lock_transaction, CommitTransaction, ContractExecutionTransaction as ContractExecutionTx,
    RefundTransaction,
};
use anyhow::{bail, Context, Result};
use bdk::bitcoin::hashes::hex::ToHex;
use bdk::bitcoin::util::bip143::SigHashCache;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::{Address, Amount, PublicKey, SigHashType, Transaction, TxOut};
use bdk::database::BatchDatabase;
use bdk::descriptor::Descriptor;
use bdk::miniscript::descriptor::Wsh;
use bdk::miniscript::DescriptorTrait;
use bdk::wallet::AddressIndex;
use bdk::FeeRate;
use itertools::Itertools;
use secp256k1_zkp::{self, schnorrsig, EcdsaAdaptorSignature, SecretKey, Signature, SECP256K1};
use std::collections::HashMap;
use std::iter::FromIterator;
use std::ops::RangeInclusive;

mod sighash_ext;
mod transaction_ext;
mod transactions;
mod txin_ext;

/// Static script to be used to create lock tx
const DUMMY_2OF2_MULTISIG: &str =
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
                DUMMY_2OF2_MULTISIG.parse().expect("Should be valid script"),
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

/// Build all the transactions and some of the signatures and
/// encrypted signatures needed to perform the CFD protocol.
///
/// # Arguments
///
/// * `maker` - The initial parameters of the maker.
/// * `maker_punish_params` - The punish parameters of the maker.
/// * `taker` - The initial parameters of the taker.
/// * `taker_punish_params` - The punish parameters of the taker.
/// * `oracle_pk` - The public key of the oracle.
/// * `nonce_pks` - One R-value per price digit signed by the oracle. Their order matches a big
///   endian encoding of the price.
/// * `refund_timelock` - Relative timelock of the refund transaction with respect to the commit
///   transaction.
/// * `payouts` - All the possible ways in which the contract can be settled, according to the
///   conditions of the bet.
/// * `identity_sk` - The secret key of the caller, used to sign and encsign different transactions.
pub fn create_cfd_transactions(
    (maker, maker_punish_params): (PartyParams, PunishParams),
    (taker, taker_punish_params): (PartyParams, PunishParams),
    (oracle_pk, nonce_pks): (schnorrsig::PublicKey, &[schnorrsig::PublicKey]),
    refund_timelock: u32,
    payouts: Vec<Payout>,
    identity_sk: SecretKey,
) -> Result<CfdTransactions> {
    let lock_tx = lock_transaction(
        maker.lock_psbt.clone(),
        taker.lock_psbt.clone(),
        maker.identity_pk,
        taker.identity_pk,
        maker.lock_amount + taker.lock_amount,
    );

    build_cfds(
        lock_tx,
        (
            maker.identity_pk,
            maker.lock_amount,
            maker.address,
            maker_punish_params,
        ),
        (
            taker.identity_pk,
            taker.lock_amount,
            taker.address,
            taker_punish_params,
        ),
        (oracle_pk, nonce_pks),
        refund_timelock,
        payouts,
        identity_sk,
    )
}

pub fn renew_cfd_transactions(
    lock_tx: PartiallySignedTransaction,
    (maker_pk, maker_lock_amount, maker_address, maker_punish_params): (
        PublicKey,
        Amount,
        Address,
        PunishParams,
    ),
    (taker_pk, taker_lock_amount, taker_address, taker_punish_params): (
        PublicKey,
        Amount,
        Address,
        PunishParams,
    ),
    (oracle_pk, nonce_pks): (schnorrsig::PublicKey, &[schnorrsig::PublicKey]),
    refund_timelock: u32,
    payouts: Vec<Payout>,
    identity_sk: SecretKey,
) -> Result<CfdTransactions> {
    build_cfds(
        lock_tx,
        (
            maker_pk,
            maker_lock_amount,
            maker_address,
            maker_punish_params,
        ),
        (
            taker_pk,
            taker_lock_amount,
            taker_address,
            taker_punish_params,
        ),
        (oracle_pk, nonce_pks),
        refund_timelock,
        payouts,
        identity_sk,
    )
}

fn build_cfds(
    lock_tx: PartiallySignedTransaction,
    (maker_pk, maker_lock_amount, maker_address, maker_punish_params): (
        PublicKey,
        Amount,
        Address,
        PunishParams,
    ),
    (taker_pk, taker_lock_amount, taker_address, taker_punish_params): (
        PublicKey,
        Amount,
        Address,
        PunishParams,
    ),
    (oracle_pk, nonce_pks): (schnorrsig::PublicKey, &[schnorrsig::PublicKey]),
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

    let commit_tx = CommitTransaction::new(
        &lock_tx.global.unsigned_tx,
        (
            maker_pk,
            maker_punish_params.revocation_pk,
            maker_punish_params.publish_pk,
        ),
        (
            taker_pk,
            taker_punish_params.revocation_pk,
            taker_punish_params.publish_pk,
        ),
    )
    .context("cannot build commit tx")?;

    let identity_pk = secp256k1_zkp::PublicKey::from_secret_key(SECP256K1, &identity_sk);
    let commit_encsig = if identity_pk == maker_pk.key {
        commit_tx.encsign(identity_sk, &taker_punish_params.publish_pk)
    } else if identity_pk == taker_pk.key {
        commit_tx.encsign(identity_sk, &maker_punish_params.publish_pk)
    } else {
        bail!("identity sk does not belong to taker or maker")
    };

    let refund = {
        let tx = RefundTransaction::new(
            &commit_tx,
            refund_timelock,
            &maker_address,
            &taker_address,
            maker_lock_amount,
            taker_lock_amount,
        );

        let sighash = tx.sighash().to_message();
        let sig = SECP256K1.sign(&sighash, &identity_sk);

        (tx.into_inner(), sig)
    };

    let cets = payouts
        .into_iter()
        .map(|payout| {
            let cet = ContractExecutionTx::new(
                &commit_tx,
                payout.clone(),
                &maker_address,
                &taker_address,
                nonce_pks,
                CET_TIMELOCK,
            )?;

            let encsig = cet.encsign(identity_sk, &oracle_pk)?;

            Ok((cet.into_inner(), encsig, payout.digits))
        })
        .collect::<Result<Vec<_>>>()
        .context("cannot build and sign all cets")?;

    Ok(CfdTransactions {
        lock: lock_tx,
        commit: (commit_tx.into_inner(), commit_encsig),
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

    // raw script:
    // or(and(pk(maker_own_pk),pk(taker_own_pk)),or(and(pk(maker_own_pk),and(pk(taker_publish_pk),
    // pk(taker_rev_pk))),and(pk(taker_own_pk),and(pk(maker_publish_pk),pk(maker_rev_pk)))))
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
    (pk_0, sig_0): (PublicKey, Signature),
    (pk_1, sig_1): (PublicKey, Signature),
) -> Result<Transaction> {
    let satisfier = HashMap::from_iter(vec![
        (pk_0, (sig_0, SigHashType::All)),
        (pk_1, (sig_1, SigHashType::All)),
    ]);

    let input = tx
        .input
        .iter_mut()
        .exactly_one()
        .expect("all spend transactions to have one input");
    spent_descriptor.satisfy(input, satisfier)?;

    Ok(tx)
}

#[derive(Clone)]
pub struct PartyParams {
    pub lock_psbt: PartiallySignedTransaction,
    pub identity_pk: PublicKey,
    pub lock_amount: Amount,
    pub address: Address,
}

#[derive(Debug, Copy, Clone)]
pub struct PunishParams {
    pub revocation_pk: PublicKey,
    pub publish_pk: PublicKey,
}

#[derive(Debug, Clone)]
pub struct CfdTransactions {
    pub lock: PartiallySignedTransaction,
    pub commit: (Transaction, EcdsaAdaptorSignature),
    pub cets: Vec<(Transaction, EcdsaAdaptorSignature, interval::Digits)>,
    pub refund: (Transaction, Signature),
}

#[derive(Debug, Clone)]
pub struct Payout {
    digits: interval::Digits,
    maker_amount: Amount,
    taker_amount: Amount,
}

impl Payout {
    pub fn new(
        range: RangeInclusive<u64>,
        maker_amount: Amount,
        taker_amount: Amount,
    ) -> Result<Vec<Self>> {
        let digits = interval::Digits::new(range).context("invalid interval")?;
        Ok(digits
            .into_iter()
            .map(|digits| Self {
                digits,
                maker_amount,
                taker_amount,
            })
            .collect())
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

/// Compute a signature point for the given oracle public key, announcement nonce public key and
/// message.
fn compute_signature_point(
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

    let hash = crate::oracle::msg_hash(oracle_pk, nonce_pk, msg);
    let mut oracle_pk = schnorr_pubkey_to_pubkey(oracle_pk)?;
    oracle_pk.mul_assign(SECP256K1, &hash)?;
    let nonce_pk = schnorr_pubkey_to_pubkey(nonce_pk)?;
    Ok(nonce_pk.combine(&oracle_pk)?)
}

pub fn compute_adaptor_point(
    oracle_pk: &schnorrsig::PublicKey,
    msg_nonce_pairs: &[(Vec<u8>, schnorrsig::PublicKey)],
) -> Result<secp256k1_zkp::PublicKey> {
    let sig_points = msg_nonce_pairs
        .iter()
        .map(|(msg, nonce_pk)| compute_signature_point(oracle_pk, nonce_pk, msg))
        .collect::<Result<Vec<_>>>()?;
    let adaptor_point =
        secp256k1_zkp::PublicKey::combine_keys(sig_points.iter().collect::<Vec<_>>().as_slice())?;

    Ok(adaptor_point)
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
        let payouts = Payout::new(
            0..=10_000,
            Amount::from_sat(orig_maker_amount),
            Amount::from_sat(orig_taker_amount),
        )
        .unwrap();
        let fee = 100;

        for payout in payouts {
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
        let payouts = Payout::new(
            0..=10_000,
            Amount::from_sat(orig_maker_amount),
            Amount::from_sat(orig_taker_amount),
        )
        .unwrap();
        let fee = 100;

        for payout in payouts {
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
}
