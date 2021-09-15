use crate::db::{
    insert_cfd, insert_cfd_offer, insert_new_cfd_state_by_offer_id, load_all_cfds,
    load_offer_by_id, Origin,
};
use crate::model::cfd::{
    AsBlocks, Cfd, CfdOffer, CfdOfferId, CfdState, CfdStateCommon, FinalizedCfd,
};
use crate::model::Usd;
use crate::wire;
use crate::wire::{AdaptorSignature, Msg0, Msg1, SetupMsg};
use anyhow::{Context, Result};
use bdk::bitcoin::secp256k1::{schnorrsig, SecretKey, Signature, SECP256K1};
use bdk::bitcoin::{self, Amount, PublicKey, Transaction, Txid};
use bdk::database::BatchDatabase;
use bdk::descriptor::Descriptor;
use cfd_protocol::{
    commit_descriptor, compute_signature_point, create_cfd_transactions, lock_descriptor,
    spending_tx_sighash, EcdsaAdaptorSignature, PartyParams, PunishParams, WalletExt,
};
use core::panic;
use futures::Future;
use std::collections::HashMap;
use std::time::SystemTime;
use tokio::sync::{mpsc, watch};

/// A factor to be added to the CFD offer term for calculating the refund timelock.
///
/// The refund timelock is important in case the oracle disappears or never publishes a signature.
/// Ideally, both users collaboratively settle in the refund scenario. This factor is important if
/// the users do not settle collaboratively.
/// `1.5` times the term as defined in CFD offer should be safe in the extreme case where a user
/// publishes the commit transaction right after the contract was initialized. In this case, the  
/// oracle still has `1.0 * cfdoffer.term` time to attest and no one can publish the refund
/// transaction.
/// The downside is that if the oracle disappears: the users would only notice at the end
/// of the cfd term. In this case the users has to wait for another `1.5` times of the
/// term to get his funds back.
pub const REFUND_THRESHOLD: f32 = 1.5;

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Command {
    TakeOffer { offer_id: CfdOfferId, quantity: Usd },
    NewOffer(Option<CfdOffer>),
    OfferAccepted(CfdOfferId),
    IncProtocolMsg(SetupMsg),
    CfdSetupCompleted(FinalizedCfd),
}

pub fn new<B, D>(
    db: sqlx::SqlitePool,
    wallet: bdk::Wallet<B, D>,
    oracle_pk: schnorrsig::PublicKey,
    cfd_feed_actor_inbox: watch::Sender<Vec<Cfd>>,
    offer_feed_actor_inbox: watch::Sender<Option<CfdOffer>>,
    out_msg_maker_inbox: mpsc::UnboundedSender<wire::TakerToMaker>,
) -> (impl Future<Output = ()>, mpsc::UnboundedSender<Command>)
where
    D: BatchDatabase,
{
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut current_contract_setup = None;

    let actor = {
        let sender = sender.clone();

        async move {
            // populate the CFD feed with existing CFDs
            let mut conn = db.acquire().await.unwrap();
            cfd_feed_actor_inbox
                .send(load_all_cfds(&mut conn).await.unwrap())
                .unwrap();

            while let Some(message) = receiver.recv().await {
                match message {
                    Command::TakeOffer { offer_id, quantity } => {
                        let mut conn = db.acquire().await.unwrap();

                        let current_offer = load_offer_by_id(offer_id, &mut conn).await.unwrap();

                        println!("Accepting current offer: {:?}", &current_offer);

                        let cfd = Cfd::new(
                            current_offer.clone(),
                            quantity,
                            CfdState::PendingTakeRequest {
                                common: CfdStateCommon {
                                    transition_timestamp: SystemTime::now(),
                                },
                            },
                            current_offer.position.counter_position(),
                        );

                        insert_cfd(cfd, &mut conn).await.unwrap();

                        cfd_feed_actor_inbox
                            .send(load_all_cfds(&mut conn).await.unwrap())
                            .unwrap();
                        out_msg_maker_inbox
                            .send(wire::TakerToMaker::TakeOffer { offer_id, quantity })
                            .unwrap();
                    }
                    Command::NewOffer(Some(offer)) => {
                        let mut conn = db.acquire().await.unwrap();
                        insert_cfd_offer(&offer, &mut conn, Origin::Theirs)
                            .await
                            .unwrap();
                        offer_feed_actor_inbox.send(Some(offer)).unwrap();
                    }

                    Command::NewOffer(None) => {
                        offer_feed_actor_inbox.send(None).unwrap();
                    }
                    Command::OfferAccepted(offer_id) => {
                        let mut conn = db.acquire().await.unwrap();
                        insert_new_cfd_state_by_offer_id(
                            offer_id,
                            CfdState::ContractSetup {
                                common: CfdStateCommon {
                                    transition_timestamp: SystemTime::now(),
                                },
                            },
                            &mut conn,
                        )
                        .await
                        .unwrap();

                        cfd_feed_actor_inbox
                            .send(load_all_cfds(&mut conn).await.unwrap())
                            .unwrap();

                        let (sk, pk) = crate::keypair::new(&mut rand::thread_rng());

                        let taker_params = wallet
                            .build_party_params(bitcoin::Amount::ZERO, pk) // TODO: Load correct quantity from DB
                            .unwrap();

                        let cfd = load_offer_by_id(offer_id, &mut conn).await.unwrap();

                        let (actor, inbox) = setup_contract(
                            {
                                let inbox = out_msg_maker_inbox.clone();
                                move |msg| inbox.send(wire::TakerToMaker::Protocol(msg)).unwrap()
                            },
                            taker_params,
                            sk,
                            oracle_pk,
                            cfd,
                        );

                        tokio::spawn({
                            let sender = sender.clone();

                            async move {
                                sender
                                    .send(Command::CfdSetupCompleted(actor.await))
                                    .unwrap()
                            }
                        });
                        current_contract_setup = Some(inbox);
                    }
                    Command::IncProtocolMsg(msg) => {
                        let inbox = match &current_contract_setup {
                            None => panic!("whoops"),
                            Some(inbox) => inbox,
                        };

                        inbox.send(msg).unwrap();
                    }
                    Command::CfdSetupCompleted(_finalized_cfd) => {
                        todo!("but what?")
                    }
                }
            }
        }
    };

    (actor, sender)
}

/// Given an initial set of parameters, sets up the CFD contract with the maker.
///
/// Returns the [`FinalizedCfd`] which contains the lock transaction, ready to be signed and sent to
/// the maker. Signing of the lock transaction is not included in this function because we want the
/// actor above to own the wallet.
fn setup_contract(
    send_to_maker: impl Fn(SetupMsg),
    taker: PartyParams,
    sk: SecretKey,
    oracle_pk: schnorrsig::PublicKey,
    offer: CfdOffer,
) -> (
    impl Future<Output = FinalizedCfd>,
    mpsc::UnboundedSender<SetupMsg>,
) {
    let (sender, mut receiver) = mpsc::unbounded_channel::<SetupMsg>();

    let actor = async move {
        let (rev_sk, rev_pk) = crate::keypair::new(&mut rand::thread_rng());
        let (publish_sk, publish_pk) = crate::keypair::new(&mut rand::thread_rng());

        let taker_punish = PunishParams {
            revocation_pk: rev_pk,
            publish_pk,
        };
        send_to_maker(SetupMsg::Msg0(Msg0::from((taker.clone(), taker_punish))));

        let msg0 = receiver.recv().await.unwrap().try_into_msg0().unwrap();
        let (maker, maker_punish) = msg0.into();

        let taker_cfd_txs = create_cfd_transactions(
            (maker.clone(), maker_punish),
            (taker.clone(), taker_punish),
            oracle_pk,
            offer.term.mul_f32(REFUND_THRESHOLD).as_blocks().ceil() as u32,
            vec![],
            sk,
        )
        .unwrap();

        send_to_maker(SetupMsg::Msg1(Msg1::from(taker_cfd_txs.clone())));
        let msg1 = receiver.recv().await.unwrap().try_into_msg1().unwrap();

        let lock_desc = lock_descriptor(maker.identity_pk, taker.identity_pk);

        let lock_amount = maker.lock_amount + taker.lock_amount;

        let commit_desc = commit_descriptor(
            (
                maker.identity_pk,
                maker_punish.revocation_pk,
                maker_punish.publish_pk,
            ),
            (taker.identity_pk, rev_pk, publish_pk),
        );

        let taker_cets = taker_cfd_txs.cets;
        let commit_tx = taker_cfd_txs.commit.0.clone();

        let commit_amount = Amount::from_sat(commit_tx.output[0].value);

        verify_adaptor_signature(
            &commit_tx,
            &lock_desc,
            lock_amount,
            &msg1.commit,
            &taker_punish.publish_pk,
            &maker.identity_pk,
        )
        .unwrap();

        verify_cets(
            &oracle_pk,
            &maker,
            &taker_cets,
            &msg1.cets,
            &commit_desc,
            commit_amount,
        )
        .unwrap();

        let lock_tx = taker_cfd_txs.lock;
        let refund_tx = taker_cfd_txs.refund.0;

        verify_signature(
            &refund_tx,
            &commit_desc,
            commit_amount,
            &msg1.refund,
            &maker.identity_pk,
        )
        .unwrap();

        let mut cet_by_id = taker_cets
            .into_iter()
            .map(|(tx, _, msg, _)| (tx.txid(), (tx, msg)))
            .collect::<HashMap<_, _>>();

        FinalizedCfd {
            identity: sk,
            revocation: rev_sk,
            publish: publish_sk,
            lock: lock_tx,
            commit: (commit_tx, *msg1.commit),
            cets: msg1
                .cets
                .into_iter()
                .map(|(txid, sig)| {
                    let (cet, msg) = cet_by_id.remove(&txid).expect("unknown CET");

                    (cet, *sig, msg)
                })
                .collect::<Vec<_>>(),
            refund: (refund_tx, msg1.refund),
        }
    };

    (actor, sender)
}

fn verify_cets(
    oracle_pk: &schnorrsig::PublicKey,
    maker: &PartyParams,
    taker_cets: &[(
        Transaction,
        EcdsaAdaptorSignature,
        Vec<u8>,
        schnorrsig::PublicKey,
    )],
    cets: &[(Txid, AdaptorSignature)],
    commit_desc: &Descriptor<PublicKey>,
    commit_amount: Amount,
) -> Result<()> {
    for (tx, _, msg, nonce_pk) in taker_cets.iter() {
        let maker_encsig = cets
            .iter()
            .find_map(|(txid, encsig)| (txid == &tx.txid()).then(|| encsig))
            .expect("one encsig per cet, per party");

        verify_cet_encsig(
            tx,
            maker_encsig,
            msg,
            &maker.identity_pk,
            (oracle_pk, nonce_pk),
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
    encsig: &AdaptorSignature,
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
    encsig: &AdaptorSignature,
    msg: &[u8],
    pk: &PublicKey,
    (oracle_pk, nonce_pk): (&schnorrsig::PublicKey, &schnorrsig::PublicKey),
    spent_descriptor: &Descriptor<PublicKey>,
    spent_amount: Amount,
) -> Result<()> {
    let sig_point = compute_signature_point(oracle_pk, nonce_pk, msg)
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
