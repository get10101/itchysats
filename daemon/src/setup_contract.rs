use crate::model::cfd::{Cet, Cfd, Dlc, RevokedCommit, Role};
use crate::model::OracleEventId;
use crate::wallet::Wallet;
use crate::wire::{
    Msg0, Msg1, Msg2, RollOverMsg, RollOverMsg0, RollOverMsg1, RollOverMsg2, SetupMsg,
};
use crate::{model, oracle, payout_curve};
use anyhow::{Context, Result};
use bdk::bitcoin::secp256k1::{schnorrsig, Signature, SECP256K1};
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::{Amount, PublicKey, Transaction};
use bdk::descriptor::Descriptor;
use bdk::miniscript::DescriptorTrait;
use cfd_protocol::secp256k1_zkp::EcdsaAdaptorSignature;
use cfd_protocol::{
    commit_descriptor, compute_adaptor_pk, create_cfd_transactions, interval, lock_descriptor,
    renew_cfd_transactions, secp256k1_zkp, spending_tx_sighash, Announcement, PartyParams,
    PunishParams,
};
use futures::stream::FusedStream;
use futures::{Sink, SinkExt, StreamExt};
use std::collections::HashMap;
use std::iter::FromIterator;
use std::ops::RangeInclusive;

/// Given an initial set of parameters, sets up the CFD contract with
/// the other party.
///
/// TODO: Replace `nonce_pks` argument with set of
/// `daemon::oracle::Announcement`, which can be mapped into
/// `cfd_protocol::Announcement`.
pub async fn new(
    mut sink: impl Sink<SetupMsg, Error = anyhow::Error> + Unpin,
    mut stream: impl FusedStream<Item = SetupMsg> + Unpin,
    (oracle_pk, announcement): (schnorrsig::PublicKey, Announcement),
    cfd: Cfd,
    wallet: Wallet,
    role: Role,
) -> Result<Dlc> {
    let (sk, pk) = crate::keypair::new(&mut rand::thread_rng());
    let (rev_sk, rev_pk) = crate::keypair::new(&mut rand::thread_rng());
    let (publish_sk, publish_pk) = crate::keypair::new(&mut rand::thread_rng());

    let margin = cfd.margin().context("Failed to calculate margin")?;
    let own_params = wallet
        .build_party_params(margin, pk)
        .await
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
        .await
        .try_into_msg0()
        .context("Failed to read Msg0")?;

    let (other, other_punish) = msg0.into();

    let params = AllParams::new(own_params, own_punish, other, other_punish, role);

    if params.other.lock_amount != cfd.counterparty_margin()? {
        anyhow::bail!(
            "Amounts sent by counterparty don't add up, expected margin {} but got {}",
            cfd.counterparty_margin()?,
            params.other.lock_amount
        )
    }

    let payouts = HashMap::from_iter([(
        announcement.clone(),
        payout_curve::calculate(
            cfd.order.price,
            cfd.quantity_usd,
            params.maker().lock_amount,
            (params.taker().lock_amount, cfd.order.leverage),
        )?,
    )]);

    let own_cfd_txs = create_cfd_transactions(
        (params.maker().clone(), *params.maker_punish()),
        (params.taker().clone(), *params.taker_punish()),
        oracle_pk,
        (
            model::cfd::Cfd::CET_TIMELOCK,
            cfd.refund_timelock_in_blocks(),
        ),
        payouts,
        sk,
    )
    .context("Failed to create CFD transactions")?;

    sink.send(SetupMsg::Msg1(Msg1::from(own_cfd_txs.clone())))
        .await
        .context("Failed to send Msg1")?;

    let msg1 = stream
        .select_next_some()
        .await
        .try_into_msg1()
        .context("Failed to read Msg1")?;

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

    for own_grouped_cets in &own_cets {
        let other_cets = msg1
            .cets
            .get(&own_grouped_cets.event.id)
            .context("Expect event to exist in msg")?;

        verify_cets(
            (&oracle_pk, &own_grouped_cets.event.nonce_pks),
            &params.other,
            own_grouped_cets.cets.as_slice(),
            other_cets.as_slice(),
            &commit_desc,
            commit_amount,
        )
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

    let mut signed_lock_tx = wallet.sign(lock_tx).await?;
    sink.send(SetupMsg::Msg2(Msg2 {
        signed_lock: signed_lock_tx.clone(),
    }))
    .await
    .context("Failed to send Msg2")?;
    let msg2 = stream
        .select_next_some()
        .await
        .try_into_msg2()
        .context("Failed to read Msg2")?;
    signed_lock_tx
        .merge(msg2.signed_lock)
        .context("Failed to merge lock PSBTs")?;

    // TODO: In case we sign+send but never receive (the signed lock_tx from the other party) we
    // need some fallback handling (after x time) to spend the outputs in a different way so the
    // other party cannot hold us hostage

    let cets = own_cets
        .into_iter()
        .map(|grouped_cets| {
            let event_id = grouped_cets.event.id;
            let other_cets = msg1
                .cets
                .get(&event_id)
                .with_context(|| format!("Counterparty CETs for event {} missing", event_id))?;
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
                            format!(
                                "Missing counterparty adaptor signature for CET corresponding to
                                 price range {:?}",
                                digits.range()
                            )
                        })?;
                    Ok(Cet {
                        tx,
                        adaptor_sig: *other_encsig,
                        range: digits.range(),
                        n_bits: digits.len(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((OracleEventId(event_id), cets))
        })
        .collect::<Result<HashMap<_, _>>>()?;

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
    })
}

pub async fn roll_over(
    mut sink: impl Sink<RollOverMsg, Error = anyhow::Error> + Unpin,
    mut stream: impl FusedStream<Item = RollOverMsg> + Unpin,
    (oracle_pk, announcement): (schnorrsig::PublicKey, oracle::Announcement),
    cfd: Cfd,
    our_role: Role,
    dlc: Dlc,
) -> Result<Dlc> {
    let sk = dlc.identity;
    let pk = PublicKey::new(secp256k1_zkp::PublicKey::from_secret_key(SECP256K1, &sk));

    let (rev_sk, rev_pk) = crate::keypair::new(&mut rand::thread_rng());
    let (publish_sk, publish_pk) = crate::keypair::new(&mut rand::thread_rng());

    let own_punish = PunishParams {
        revocation_pk: rev_pk,
        publish_pk,
    };

    sink.send(RollOverMsg::Msg0(RollOverMsg0 {
        revocation_pk: rev_pk,
        publish_pk,
    }))
    .await
    .context("Failed to send Msg0")?;
    let msg0 = stream
        .select_next_some()
        .await
        .try_into_msg0()
        .context("Failed to read Msg0")?;

    let maker_lock_amount = dlc.maker_lock_amount;
    let taker_lock_amount = dlc.taker_lock_amount;
    let payouts = HashMap::from_iter([(
        // TODO : we want to support multiple announcements
        Announcement {
            id: announcement.id.0,
            nonce_pks: announcement.nonce_pks.clone(),
        },
        payout_curve::calculate(
            cfd.order.price,
            cfd.quantity_usd,
            maker_lock_amount,
            (taker_lock_amount, cfd.order.leverage),
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
    let own_cfd_txs = renew_cfd_transactions(
        lock_tx.clone(),
        (
            maker_identity,
            maker_lock_amount,
            dlc.maker_address.clone(),
            maker_punish_params,
        ),
        (
            taker_identity,
            taker_lock_amount,
            dlc.taker_address.clone(),
            taker_punish_params,
        ),
        oracle_pk,
        (
            model::cfd::Cfd::CET_TIMELOCK,
            cfd.refund_timelock_in_blocks(),
        ),
        payouts,
        sk,
    )
    .context("Failed to create new CFD transactions")?;

    sink.send(RollOverMsg::Msg1(RollOverMsg1::from(own_cfd_txs.clone())))
        .await
        .context("Failed to send Msg1")?;

    let msg1 = stream
        .select_next_some()
        .await
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

    for own_grouped_cets in &own_cets {
        let other_cets = msg1
            .cets
            .get(&own_grouped_cets.event.id)
            .context("Expect event to exist in msg")?;

        verify_cets(
            (&oracle_pk, &announcement.nonce_pks),
            &PartyParams {
                lock_psbt: lock_tx.clone(),
                identity_pk: dlc.identity_counterparty,
                lock_amount,
                address: other_address.clone(),
            },
            own_grouped_cets.cets.as_slice(),
            other_cets.as_slice(),
            &commit_desc,
            commit_amount,
        )
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

    let cets = own_cets
        .into_iter()
        .map(|grouped_cets| {
            let event_id = grouped_cets.event.id;
            let other_cets = msg1
                .cets
                .get(&event_id)
                .with_context(|| format!("Counterparty CETs for event {} missing", event_id))?;
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
                            format!(
                                "Missing counterparty adaptor signature for CET corresponding to
                                 price range {:?}",
                                digits.range()
                            )
                        })?;
                    Ok(Cet {
                        tx,
                        adaptor_sig: *other_encsig,
                        range: digits.range(),
                        n_bits: digits.len(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((OracleEventId(event_id), cets))
        })
        .collect::<Result<HashMap<_, _>>>()?;

    // reveal revocation secrets to the other party
    sink.send(RollOverMsg::Msg2(RollOverMsg2 {
        revocation_sk: dlc.revocation,
    }))
    .await
    .context("Failed to send Msg1")?;

    let msg2 = stream
        .select_next_some()
        .await
        .try_into_msg2()
        .context("Failed to read Msg1")?;
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

fn verify_cets(
    oracle_params: (&schnorrsig::PublicKey, &[schnorrsig::PublicKey]),
    other: &PartyParams,
    own_cets: &[(Transaction, EcdsaAdaptorSignature, interval::Digits)],
    cets: &[(RangeInclusive<u64>, EcdsaAdaptorSignature)],
    commit_desc: &Descriptor<PublicKey>,
    commit_amount: Amount,
) -> Result<()> {
    for (tx, _, digits) in own_cets.iter() {
        let other_encsig = cets
            .iter()
            .find_map(|(range, encsig)| (range == &digits.range()).then(|| encsig))
            .expect("one encsig per cet, per party");

        verify_cet_encsig(
            tx,
            other_encsig,
            digits,
            &other.identity_pk,
            oracle_params,
            commit_desc,
            commit_amount,
        )
        .expect("valid maker cet encsig")
    }
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
