use crate::model::cfd::{Cfd, Dlc};
use crate::wallet::Wallet;
use crate::wire::{Msg0, Msg1, Msg2, SetupMsg};
use anyhow::{Context, Result};
use bdk::bitcoin::secp256k1::{schnorrsig, SecretKey, Signature, SECP256K1};
use bdk::bitcoin::{Amount, PublicKey, Transaction};
use bdk::descriptor::Descriptor;
use cfd_protocol::secp256k1_zkp::EcdsaAdaptorSignature;
use cfd_protocol::{
    commit_descriptor, compute_adaptor_point, create_cfd_transactions, interval, lock_descriptor,
    spending_tx_sighash, PartyParams, PunishParams,
};
use futures::Future;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use tokio::sync::mpsc;

/// Given an initial set of parameters, sets up the CFD contract with the other party.
/// Passing OwnParams identifies whether caller is the maker or the taker.
///
/// Returns the [`Dlc`] which contains the lock transaction, ready to be signed and sent to
/// the other party. Signing of the lock transaction is not included in this function because we
/// want the Cfd actor to own the wallet.
pub fn new(
    send_to_other: impl Fn(SetupMsg),
    own_params: OwnParams,
    sk: SecretKey,
    oracle_pk: schnorrsig::PublicKey,
    cfd: Cfd,
    wallet: Wallet,
) -> (impl Future<Output = Dlc>, mpsc::UnboundedSender<SetupMsg>) {
    let (sender, mut receiver) = mpsc::unbounded_channel::<SetupMsg>();

    let actor = async move {
        let (rev_sk, rev_pk) = crate::keypair::new(&mut rand::thread_rng());
        let (publish_sk, publish_pk) = crate::keypair::new(&mut rand::thread_rng());

        let params = {
            let (own, own_role) = match own_params.clone() {
                OwnParams::Maker(maker) => (maker, Role::Maker),
                OwnParams::Taker(taker) => (taker, Role::Taker),
            };

            let own_punish = PunishParams {
                revocation_pk: rev_pk,
                publish_pk,
            };
            send_to_other(SetupMsg::Msg0(Msg0::from((own.clone(), own_punish))));

            let msg0 = receiver.recv().await.unwrap().try_into_msg0().unwrap();
            let (other, other_punish) = msg0.into();

            AllParams::new(own, own_punish, other, other_punish, own_role)
        };

        if params.other.lock_amount != cfd.counterparty_margin().unwrap() {
            panic!("Sorry, have to panic 😬 - the amounts that the counterparty sent were wrong, expected {} actual {}", cfd.counterparty_margin().unwrap(), params.other.lock_amount)
        }

        let own_cfd_txs = create_cfd_transactions(
            (params.maker().clone(), *params.maker_punish()),
            (params.taker().clone(), *params.taker_punish()),
            (oracle_pk, &[]),
            cfd.refund_timelock_in_blocks(),
            vec![],
            sk,
        )
        .unwrap();

        send_to_other(SetupMsg::Msg1(Msg1::from(own_cfd_txs.clone())));
        let msg1 = receiver.recv().await.unwrap().try_into_msg1().unwrap();

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
        .unwrap();

        verify_cets(
            (&oracle_pk, &[]),
            &params.other,
            &own_cets,
            &msg1.cets,
            &commit_desc,
            commit_amount,
        )
        .unwrap();

        let lock_tx = own_cfd_txs.lock;
        let refund_tx = own_cfd_txs.refund.0;

        verify_signature(
            &refund_tx,
            &commit_desc,
            commit_amount,
            &msg1.refund,
            &params.other.identity_pk,
        )
        .unwrap();

        let mut cet_by_digits = own_cets
            .into_iter()
            .map(|(tx, _, digits)| (digits.range(), (tx, digits)))
            .collect::<HashMap<_, _>>();

        let mut signed_lock_tx = wallet.sign(lock_tx).await.unwrap();
        send_to_other(SetupMsg::Msg2(Msg2 {
            signed_lock: signed_lock_tx.clone(),
        }));
        let msg2 = receiver.recv().await.unwrap().try_into_msg2().unwrap();
        signed_lock_tx.merge(msg2.signed_lock).unwrap();

        // TODO: In case we sign+send but never receive (the signed lock_tx from the other party) we
        // need some fallback handling (after x time) to spend the outputs in a different way so the
        // other party cannot hold us hostage

        Dlc {
            identity: sk,
            revocation: rev_sk,
            publish: publish_sk,
            lock: signed_lock_tx.extract_tx(),
            commit: (commit_tx, msg1.commit),
            cets: msg1
                .cets
                .into_iter()
                .map(|(range, sig)| {
                    let (cet, digits) = cet_by_digits.remove(&range).expect("unknown CET");

                    (cet, sig, digits.range())
                })
                .collect::<Vec<_>>(),
            refund: (refund_tx, msg1.refund),
        }
    };

    (actor, sender)
}

#[derive(Clone)]
#[allow(dead_code)]
pub enum OwnParams {
    Maker(PartyParams),
    Taker(PartyParams),
}

/// Role of the actor's owner in the upcoming contract
enum Role {
    Maker,
    Taker,
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
    let msg_nonce_pairs = &digits
        .to_bytes()
        .into_iter()
        .zip(nonce_pks.iter().cloned())
        .collect::<Vec<_>>();
    let sig_point = compute_adaptor_point(oracle_pk, msg_nonce_pairs)
        .context("could not calculate signature point")?;
    verify_adaptor_signature(
        tx,
        spent_descriptor,
        spent_amount,
        encsig,
        &PublicKey::new(sig_point),
        pk,
    )
}
