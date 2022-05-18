use maia_core::secp256k1_zkp::EcdsaAdaptorSignature;
use maia_core::{interval, PartyParams};
use bdk::descriptor::Descriptor;
use maia::{compute_adaptor_pk, spending_tx_sighash};
use std::time::Duration;
use std::ops::RangeInclusive;
use anyhow::Context;
use crate::bitcoin::{PublicKey, Transaction};
use crate::{Amount, schnorrsig};
use crate::bitcoin::secp256k1::{SECP256K1, Signature};

pub async fn verify_cets(
    (oracle_pk, nonce_pks): (schnorrsig::PublicKey, Vec<schnorrsig::PublicKey>),
    other: PartyParams,
    own_cets: Vec<(Transaction, EcdsaAdaptorSignature, interval::Digits)>,
    cets: Vec<(RangeInclusive<u64>, EcdsaAdaptorSignature)>,
    commit_desc: Descriptor<PublicKey>,
    commit_amount: Amount,
) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || {
        for (tx, _, digits) in own_cets.iter() {
            let other_encsig = cets
                .iter()
                .find_map(|(range, encsig)| (range == &digits.range()).then(|| encsig))
                .with_context(|| {
                    let range = digits.range();

                    format!("no enc sig from other party for price range {range:?}",)
                })?;

            verify_cet_encsig(
                tx,
                other_encsig,
                digits,
                &other.identity_pk,
                (&oracle_pk, &nonce_pks),
                &commit_desc,
                commit_amount,
            )
            .context("enc sig on CET does not verify")?;
        }

        anyhow::Ok(())
    })
    .await??;

    Ok(())
}

pub fn verify_adaptor_signature(
    tx: &Transaction,
    spent_descriptor: &Descriptor<PublicKey>,
    spent_amount: Amount,
    encsig: &EcdsaAdaptorSignature,
    encryption_point: &PublicKey,
    pk: &PublicKey,
) -> anyhow::Result<()> {
    let sighash = spending_tx_sighash(tx, spent_descriptor, spent_amount);

    encsig
        .verify(SECP256K1, &sighash, &pk.key, &encryption_point.key)
        .context("failed to verify encsig spend tx")
}

pub fn verify_signature(
    tx: &Transaction,
    spent_descriptor: &Descriptor<PublicKey>,
    spent_amount: Amount,
    sig: &Signature,
    pk: &PublicKey,
) -> anyhow::Result<()> {
    let sighash = spending_tx_sighash(tx, spent_descriptor, spent_amount);
    SECP256K1.verify(&sighash, sig, &pk.key)?;
    Ok(())
}

fn verify_cet_encsig(
    tx: &Transaction,
    encsig: &EcdsaAdaptorSignature,
    digits: &interval::Digits,
    pk: &PublicKey,
    (oracle_pk, nonce_pks): (&schnorrsig::PublicKey, &[schnorrsig::PublicKey]),
    spent_descriptor: &Descriptor<PublicKey>,
    spent_amount: Amount,
) -> anyhow::Result<()> {
    let index_nonce_pairs = &digits
        .to_indices()
        .into_iter()
        .zip(nonce_pks.iter().cloned())
        .collect::<Vec<_>>();
    let adaptor_point = compute_adaptor_pk(oracle_pk, index_nonce_pairs)
        .context("could not calculate adaptor point")?;
    verify_adaptor_signature(
        tx,
        spent_descriptor,
        spent_amount,
        encsig,
        &PublicKey::new(adaptor_point),
        pk,
    )
}

/// Wrapper for the msg
pub fn format_expect_msg_within(msg: &str, timeout: Duration) -> String {
    let seconds = timeout.as_secs();

    format!("Expected {msg} within {seconds} seconds")
}
