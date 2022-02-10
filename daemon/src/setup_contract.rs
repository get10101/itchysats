use crate::future_ext::FutureExt;
use crate::transaction_ext::TransactionExt;
use crate::wallet;
use crate::wire::Msg0;
use crate::wire::Msg1;
use crate::wire::Msg2;
use crate::wire::Msg3;
use crate::wire::RolloverMsg;
use crate::wire::RolloverMsg0;
use crate::wire::RolloverMsg1;
use crate::wire::RolloverMsg2;
use crate::wire::RolloverMsg3;
use crate::wire::SetupMsg;
use anyhow::Context;
use anyhow::Result;
use bdk::bitcoin::secp256k1::schnorrsig;
use bdk::bitcoin::secp256k1::Signature;
use bdk::bitcoin::secp256k1::SECP256K1;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::Amount;
use bdk::bitcoin::PublicKey;
use bdk::bitcoin::Transaction;
use bdk::descriptor::Descriptor;
use bdk::miniscript::DescriptorTrait;
use bdk_ext::keypair;
use futures::stream::FusedStream;
use futures::Sink;
use futures::SinkExt;
use futures::StreamExt;
use maia::commit_descriptor;
use maia::compute_adaptor_pk;
use maia::create_cfd_transactions;
use maia::interval;
use maia::lock_descriptor;
use maia::renew_cfd_transactions;
use maia::secp256k1_zkp;
use maia::secp256k1_zkp::EcdsaAdaptorSignature;
use maia::spending_tx_sighash;
use maia::Announcement;
use maia::PartyParams;
use maia::PunishParams;
use model::cfd::Cet;
use model::cfd::Dlc;
use model::cfd::RevokedCommit;
use model::cfd::Role;
use model::cfd::RolloverParams;
use model::cfd::SetupParams;
use model::cfd::CET_TIMELOCK;
use model::olivia;
use model::payout_curve;
use std::collections::HashMap;
use std::iter::FromIterator;
use std::ops::RangeInclusive;
use std::time::Duration;
use xtra::prelude::MessageChannel;

/// How long protocol waits for the next message before giving up
const MSG_TIMEOUT: Duration = Duration::from_secs(70);

/// Given an initial set of parameters, sets up the CFD contract with
/// the other party.
#[allow(clippy::too_many_arguments)]
pub async fn new(
    mut sink: impl Sink<SetupMsg, Error = anyhow::Error> + Unpin,
    mut stream: impl FusedStream<Item = SetupMsg> + Unpin,
    (oracle_pk, announcement): (schnorrsig::PublicKey, olivia::Announcement),
    setup_params: SetupParams,
    build_party_params_channel: Box<dyn MessageChannel<wallet::BuildPartyParams>>,
    sign_channel: Box<dyn MessageChannel<wallet::Sign>>,
    role: Role,
    n_payouts: usize,
) -> Result<Dlc> {
    let (sk, pk) = keypair::new(&mut rand::thread_rng());
    let (rev_sk, rev_pk) = keypair::new(&mut rand::thread_rng());
    let (publish_sk, publish_pk) = keypair::new(&mut rand::thread_rng());

    let own_params = build_party_params_channel
        .send(wallet::BuildPartyParams {
            amount: setup_params.margin,
            identity_pk: pk,
            fee_rate: setup_params.tx_fee_rate,
        })
        .await
        .context("Failed to send message to wallet actor")?
        .context("Failed to build party params")?;

    let own_punish = PunishParams {
        revocation_pk: rev_pk,
        publish_pk,
    };

    sink.send(SetupMsg::Msg0(Msg0::from((own_params.clone(), own_punish))))
        .await
        .context("Failed to send Msg0")?;
    let msg0 = stream
        .select_next_some()
        .timeout(MSG_TIMEOUT)
        .await
        .with_context(|| format_expect_msg_within("Msg0"))?
        .try_into_msg0()
        .context("Failed to read Msg0")?;

    tracing::info!("Exchanged setup parameters");

    let (other, other_punish) = msg0.into();

    let params = AllParams::new(own_params, own_punish, other, other_punish, role);

    let expected_margin = setup_params.counterparty_margin;
    let actual_margin = params.other.lock_amount;

    if actual_margin != expected_margin {
        anyhow::bail!(
            "Amounts sent by counterparty don't add up, expected margin {expected_margin} but got {actual_margin}"
        )
    }

    let settlement_event_id = announcement.id;
    let payouts = HashMap::from_iter([(
        announcement.into(),
        payout_curve::calculate(
            setup_params.price,
            setup_params.quantity,
            setup_params.leverage,
            n_payouts,
            setup_params.fee_account.settle(),
        )?,
    )]);

    let own_cfd_txs = tokio::task::spawn_blocking({
        let maker_params = params.maker().clone();
        let taker_params = params.taker().clone();
        let maker_punish = *params.maker_punish();
        let taker_punish = *params.taker_punish();

        move || {
            create_cfd_transactions(
                (maker_params, maker_punish),
                (taker_params, taker_punish),
                oracle_pk,
                (CET_TIMELOCK, setup_params.refund_timelock),
                payouts,
                sk,
                setup_params.tx_fee_rate.to_u32(),
            )
        }
    })
    .await?
    .context("Failed to create CFD transactions")?;

    tracing::info!("Created CFD transactions");

    sink.send(SetupMsg::Msg1(Msg1::from(own_cfd_txs.clone())))
        .await
        .context("Failed to send Msg1")?;

    let msg1 = stream
        .select_next_some()
        .timeout(MSG_TIMEOUT)
        .await
        .with_context(|| format_expect_msg_within("Msg1"))?
        .try_into_msg1()
        .context("Failed to read Msg1")?;

    tracing::info!("Exchanged CFD transactions");

    let lock_desc = lock_descriptor(params.maker().identity_pk, params.taker().identity_pk);

    let lock_amount = params.maker().lock_amount + params.taker().lock_amount;

    let commit_desc = commit_descriptor(
        (
            params.maker().identity_pk,
            params.maker_punish().revocation_pk,
            params.maker_punish().publish_pk,
        ),
        (
            params.taker().identity_pk,
            params.taker_punish().revocation_pk,
            params.taker_punish().publish_pk,
        ),
    );

    let own_cets = own_cfd_txs.cets;
    let commit_tx = own_cfd_txs.commit.0.clone();

    let commit_amount = Amount::from_sat(commit_tx.output[0].value);

    verify_adaptor_signature(
        &commit_tx,
        &lock_desc,
        lock_amount,
        &msg1.commit,
        &params.own_punish.publish_pk,
        &params.other.identity_pk,
    )
    .context("Commit adaptor signature does not verify")?;

    for own_grouped_cets in own_cets.clone() {
        let other_cets = msg1
            .cets
            .get(&own_grouped_cets.event.id)
            .cloned()
            .context("Expect event to exist in msg")?;

        verify_cets(
            (oracle_pk, own_grouped_cets.event.nonce_pks.clone()),
            params.other.clone(),
            own_grouped_cets.cets,
            other_cets,
            commit_desc.clone(),
            commit_amount,
        )
        .await
        .context("CET signatures don't verify")?;
    }

    let lock_tx = own_cfd_txs.lock;
    let refund_tx = own_cfd_txs.refund.0;

    verify_signature(
        &refund_tx,
        &commit_desc,
        commit_amount,
        &msg1.refund,
        &params.other.identity_pk,
    )
    .context("Refund signature does not verify")?;

    tracing::info!("Verified all signatures");

    let mut signed_lock_tx = sign_channel
        .send(wallet::Sign { psbt: lock_tx })
        .await
        .context("Failed to send message to wallet actor")?
        .context("Failed to sign transaction")?;
    sink.send(SetupMsg::Msg2(Msg2 {
        signed_lock: signed_lock_tx.clone(),
    }))
    .await
    .context("Failed to send Msg2")?;
    let msg2 = stream
        .select_next_some()
        .timeout(MSG_TIMEOUT)
        .await
        .with_context(|| format_expect_msg_within("Msg2"))?
        .try_into_msg2()
        .context("Failed to read Msg2")?;
    signed_lock_tx
        .merge(msg2.signed_lock)
        .context("Failed to merge lock PSBTs")?;

    tracing::info!("Exchanged signed lock transaction");

    // TODO: In case we sign+send but never receive (the signed lock_tx from the other party) we
    // need some fallback handling (after x time) to spend the outputs in a different way so the
    // other party cannot hold us hostage

    let maker_script_pubkey = params.maker().address.script_pubkey();
    let taker_script_pubkey = params.taker().address.script_pubkey();
    let cets = tokio::task::spawn_blocking(move || {
        own_cets
            .into_iter()
            .map(|grouped_cets| {
                let event_id = grouped_cets.event.id;
                let other_cets = msg1
                    .cets
                    .get(&event_id)
                    .with_context(|| format!("Counterparty CETs for event {event_id} missing"))?;
                let cets = grouped_cets
                    .cets
                    .into_iter()
                    .map(|(tx, _, digits)| {
                        let other_encsig = other_cets
                            .iter()
                            .find_map(|(other_range, other_encsig)| {
                                (other_range == &digits.range()).then(|| other_encsig)
                            })
                            .with_context(|| {
                                let range = digits.range();

                                format!(
                                    "Missing counterparty adaptor signature for CET corresponding to price range {range:?}",
                                )
                            })?;

                        let maker_amount = tx.find_output_amount(&maker_script_pubkey).unwrap_or_default();
                        let taker_amount = tx.find_output_amount(&taker_script_pubkey).unwrap_or_default();

                        Ok(Cet {
                            maker_amount,
                            taker_amount,
                            adaptor_sig: *other_encsig,
                            range: digits.range(),
                            n_bits: digits.len(),
                            txid: tx.txid(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok((event_id.parse()?, cets))
            })
            .collect::<Result<HashMap<_, _>>>()
    })
    .await??;

    // TODO: Remove send- and receiving ACK messages once we are able to handle incomplete DLC
    // monitoring
    sink.send(SetupMsg::Msg3(Msg3))
        .await
        .context("Failed to send Msg3")?;
    let _ = stream
        .select_next_some()
        .timeout(MSG_TIMEOUT)
        .await
        .with_context(|| format_expect_msg_within("Msg3"))?
        .try_into_msg3()
        .context("Failed to read Msg3")?;

    Ok(Dlc {
        identity: sk,
        identity_counterparty: params.other.identity_pk,
        revocation: rev_sk,
        revocation_pk_counterparty: other_punish.revocation_pk,
        publish: publish_sk,
        publish_pk_counterparty: other_punish.publish_pk,
        maker_address: params.maker().address.clone(),
        taker_address: params.taker().address.clone(),
        lock: (signed_lock_tx.extract_tx(), lock_desc),
        commit: (commit_tx, msg1.commit, commit_desc),
        cets,
        refund: (refund_tx, msg1.refund),
        maker_lock_amount: params.maker().lock_amount,
        taker_lock_amount: params.taker().lock_amount,
        revoked_commit: Vec::new(),
        settlement_event_id,
        refund_timelock: setup_params.refund_timelock,
    })
}

pub async fn roll_over(
    mut sink: impl Sink<RolloverMsg, Error = anyhow::Error> + Unpin,
    mut stream: impl FusedStream<Item = RolloverMsg> + Unpin,
    (oracle_pk, announcement): (schnorrsig::PublicKey, olivia::Announcement),
    rollover_params: RolloverParams,
    our_role: Role,
    dlc: Dlc,
    n_payouts: usize,
) -> Result<Dlc> {
    let sk = dlc.identity;
    let pk = PublicKey::new(secp256k1_zkp::PublicKey::from_secret_key(SECP256K1, &sk));

    let (rev_sk, rev_pk) = keypair::new(&mut rand::thread_rng());
    let (publish_sk, publish_pk) = keypair::new(&mut rand::thread_rng());

    let own_punish = PunishParams {
        revocation_pk: rev_pk,
        publish_pk,
    };

    sink.send(RolloverMsg::Msg0(RolloverMsg0 {
        revocation_pk: rev_pk,
        publish_pk,
    }))
    .await
    .context("Failed to send Msg0")?;
    let msg0 = stream
        .select_next_some()
        .timeout(MSG_TIMEOUT)
        .await
        .with_context(|| format_expect_msg_within("Msg0"))?
        .try_into_msg0()
        .context("Failed to read Msg0")?;

    let maker_lock_amount = dlc.maker_lock_amount;
    let taker_lock_amount = dlc.taker_lock_amount;
    let payouts = HashMap::from_iter([(
        Announcement {
            id: announcement.id.to_string(),
            nonce_pks: announcement.nonce_pks.clone(),
        },
        payout_curve::calculate(
            rollover_params.price,
            rollover_params.quantity,
            rollover_params.leverage,
            n_payouts,
            rollover_params.fee_account.settle(),
        )?,
    )]);

    // unsign lock tx because PartiallySignedTransaction needs an unsigned tx
    let mut unsigned_lock_tx = dlc.lock.0.clone();
    unsigned_lock_tx
        .input
        .iter_mut()
        .for_each(|input| input.witness.clear());

    let lock_tx = PartiallySignedTransaction::from_unsigned_tx(unsigned_lock_tx)?;
    let other_punish_params = PunishParams {
        revocation_pk: msg0.revocation_pk,
        publish_pk: msg0.publish_pk,
    };
    let ((maker_identity, maker_punish_params), (taker_identity, taker_punish_params)) =
        match our_role {
            Role::Maker => (
                (pk, own_punish),
                (dlc.identity_counterparty, other_punish_params),
            ),
            Role::Taker => (
                (dlc.identity_counterparty, other_punish_params),
                (pk, own_punish),
            ),
        };
    let own_cfd_txs = tokio::task::spawn_blocking({
        let maker_address = dlc.maker_address.clone();
        let taker_address = dlc.taker_address.clone();
        let lock_tx = lock_tx.clone();

        move || {
            renew_cfd_transactions(
                lock_tx,
                (
                    maker_identity,
                    maker_lock_amount,
                    maker_address,
                    maker_punish_params,
                ),
                (
                    taker_identity,
                    taker_lock_amount,
                    taker_address,
                    taker_punish_params,
                ),
                oracle_pk,
                (CET_TIMELOCK, rollover_params.refund_timelock),
                payouts,
                sk,
                rollover_params.fee_rate.to_u32(),
            )
        }
    })
    .await?
    .context("Failed to create new CFD transactions")?;

    sink.send(RolloverMsg::Msg1(RolloverMsg1::from(own_cfd_txs.clone())))
        .await
        .context("Failed to send Msg1")?;

    let msg1 = stream
        .select_next_some()
        .timeout(MSG_TIMEOUT)
        .await
        .with_context(|| format_expect_msg_within("Msg1"))?
        .try_into_msg1()
        .context("Failed to read Msg1")?;

    let lock_amount = taker_lock_amount + maker_lock_amount;

    let commit_desc = commit_descriptor(
        (
            maker_identity,
            maker_punish_params.revocation_pk,
            maker_punish_params.publish_pk,
        ),
        (
            taker_identity,
            taker_punish_params.revocation_pk,
            taker_punish_params.publish_pk,
        ),
    );

    let own_cets = own_cfd_txs.cets;
    let commit_tx = own_cfd_txs.commit.0.clone();

    let commit_amount = Amount::from_sat(commit_tx.output[0].value);

    verify_adaptor_signature(
        &commit_tx,
        &dlc.lock.1,
        lock_amount,
        &msg1.commit,
        &publish_pk,
        &dlc.identity_counterparty,
    )
    .context("Commit adaptor signature does not verify")?;

    let other_address = match our_role {
        Role::Maker => dlc.taker_address.clone(),
        Role::Taker => dlc.maker_address.clone(),
    };

    for own_grouped_cets in own_cets.clone() {
        let other_cets = msg1
            .cets
            .get(&own_grouped_cets.event.id)
            .cloned()
            .context("Expect event to exist in msg")?;

        verify_cets(
            (oracle_pk, announcement.nonce_pks.clone()),
            PartyParams {
                lock_psbt: lock_tx.clone(),
                identity_pk: dlc.identity_counterparty,
                lock_amount,
                address: other_address.clone(),
            },
            own_grouped_cets.cets,
            other_cets,
            commit_desc.clone(),
            commit_amount,
        )
        .await
        .context("CET signatures don't verify")?;
    }

    let refund_tx = own_cfd_txs.refund.0;

    verify_signature(
        &refund_tx,
        &commit_desc,
        commit_amount,
        &msg1.refund,
        &dlc.identity_counterparty,
    )
    .context("Refund signature does not verify")?;

    let maker_script_pubkey = dlc.maker_address.script_pubkey();
    let taker_script_pubkey = dlc.taker_address.script_pubkey();
    let cets = own_cets
        .into_iter()
        .map(|grouped_cets| {
            let event_id = grouped_cets.event.id;
            let other_cets = msg1
                .cets
                .get(&event_id)
                .with_context(|| format!("Counterparty CETs for event {event_id} missing"))?;
            let cets = grouped_cets
                .cets
                .into_iter()
                .map(|(tx, _, digits)| {
                    let other_encsig = other_cets
                        .iter()
                        .find_map(|(other_range, other_encsig)| {
                            (other_range == &digits.range()).then(|| other_encsig)
                        })
                        .with_context(|| {
                            let range = digits.range();

                            format!(
                                "Missing counterparty adaptor signature for CET corresponding to
                                 price range {range:?}"
                            )
                        })?;

                    let maker_amount = tx
                        .find_output_amount(&maker_script_pubkey)
                        .unwrap_or_default();
                    let taker_amount = tx
                        .find_output_amount(&taker_script_pubkey)
                        .unwrap_or_default();

                    Ok(Cet {
                        maker_amount,
                        taker_amount,
                        adaptor_sig: *other_encsig,
                        range: digits.range(),
                        n_bits: digits.len(),
                        txid: tx.txid(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((event_id.parse()?, cets))
        })
        .collect::<Result<HashMap<_, _>>>()?;

    // reveal revocation secrets to the other party
    sink.send(RolloverMsg::Msg2(RolloverMsg2 {
        revocation_sk: dlc.revocation,
    }))
    .await
    .context("Failed to send Msg2")?;

    let msg2 = stream
        .select_next_some()
        .timeout(MSG_TIMEOUT)
        .await
        .with_context(|| format_expect_msg_within("Msg2"))?
        .try_into_msg2()
        .context("Failed to read Msg2")?;
    let revocation_sk_theirs = msg2.revocation_sk;

    {
        let derived_rev_pk = PublicKey::new(secp256k1_zkp::PublicKey::from_secret_key(
            SECP256K1,
            &revocation_sk_theirs,
        ));

        if derived_rev_pk != dlc.revocation_pk_counterparty {
            anyhow::bail!("Counterparty sent invalid revocation sk");
        }
    }

    let mut revoked_commit = dlc.revoked_commit;
    revoked_commit.push(RevokedCommit {
        encsig_ours: own_cfd_txs.commit.1,
        revocation_sk_theirs,
        publication_pk_theirs: dlc.publish_pk_counterparty,
        txid: dlc.commit.0.txid(),
        script_pubkey: dlc.commit.2.script_pubkey(),
    });

    // TODO: Remove send- and receiving ACK messages once we are able to handle incomplete DLC
    // monitoring
    sink.send(RolloverMsg::Msg3(RolloverMsg3))
        .await
        .context("Failed to send Msg3")?;
    let _ = stream
        .select_next_some()
        .timeout(MSG_TIMEOUT)
        .await
        .with_context(|| format_expect_msg_within("Msg3"))?
        .try_into_msg3()
        .context("Failed to read Msg3")?;

    Ok(Dlc {
        identity: sk,
        identity_counterparty: dlc.identity_counterparty,
        revocation: rev_sk,
        revocation_pk_counterparty: other_punish_params.revocation_pk,
        publish: publish_sk,
        publish_pk_counterparty: other_punish_params.publish_pk,
        maker_address: dlc.maker_address,
        taker_address: dlc.taker_address,
        lock: dlc.lock.clone(),
        commit: (commit_tx, msg1.commit, commit_desc),
        cets,
        refund: (refund_tx, msg1.refund),
        maker_lock_amount,
        taker_lock_amount,
        revoked_commit,
        settlement_event_id: announcement.id,
        refund_timelock: rollover_params.refund_timelock,
    })
}

/// A convenience struct for storing PartyParams and PunishParams of both
/// parties and the role of the caller.
struct AllParams {
    pub own: PartyParams,
    pub own_punish: PunishParams,
    pub other: PartyParams,
    pub other_punish: PunishParams,
    pub own_role: Role,
}

impl AllParams {
    fn new(
        own: PartyParams,
        own_punish: PunishParams,
        other: PartyParams,
        other_punish: PunishParams,
        own_role: Role,
    ) -> Self {
        Self {
            own,
            own_punish,
            other,
            other_punish,
            own_role,
        }
    }

    fn maker(&self) -> &PartyParams {
        match self.own_role {
            Role::Maker => &self.own,
            Role::Taker => &self.other,
        }
    }

    fn taker(&self) -> &PartyParams {
        match self.own_role {
            Role::Maker => &self.other,
            Role::Taker => &self.own,
        }
    }

    fn maker_punish(&self) -> &PunishParams {
        match self.own_role {
            Role::Maker => &self.own_punish,
            Role::Taker => &self.other_punish,
        }
    }
    fn taker_punish(&self) -> &PunishParams {
        match self.own_role {
            Role::Maker => &self.other_punish,
            Role::Taker => &self.own_punish,
        }
    }
}

async fn verify_cets(
    (oracle_pk, nonce_pks): (schnorrsig::PublicKey, Vec<schnorrsig::PublicKey>),
    other: PartyParams,
    own_cets: Vec<(Transaction, EcdsaAdaptorSignature, interval::Digits)>,
    cets: Vec<(RangeInclusive<u64>, EcdsaAdaptorSignature)>,
    commit_desc: Descriptor<PublicKey>,
    commit_amount: Amount,
) -> Result<()> {
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

fn verify_adaptor_signature(
    tx: &Transaction,
    spent_descriptor: &Descriptor<PublicKey>,
    spent_amount: Amount,
    encsig: &EcdsaAdaptorSignature,
    encryption_point: &PublicKey,
    pk: &PublicKey,
) -> Result<()> {
    let sighash = spending_tx_sighash(tx, spent_descriptor, spent_amount);

    encsig
        .verify(SECP256K1, &sighash, &pk.key, &encryption_point.key)
        .context("failed to verify encsig spend tx")
}

fn verify_signature(
    tx: &Transaction,
    spent_descriptor: &Descriptor<PublicKey>,
    spent_amount: Amount,
    sig: &Signature,
    pk: &PublicKey,
) -> Result<()> {
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
) -> Result<()> {
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
fn format_expect_msg_within(msg: &str) -> String {
    let seconds = MSG_TIMEOUT.as_secs();

    format!("Expected {msg} within {seconds} seconds")
}
