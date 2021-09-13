use std::collections::HashMap;
use std::time::SystemTime;

use crate::model::cfd::{Cfd, CfdOffer, CfdOfferId, CfdState, CfdStateCommon, FinalizedCfd};
use crate::model::{TakerId, Usd};
use crate::wire::{Msg0, Msg1, SetupMsg};
use crate::{db, maker_cfd_actor, maker_inc_connections_actor};
use bdk::bitcoin::secp256k1::{schnorrsig, SecretKey};
use bdk::bitcoin::{self, Amount};
use bdk::database::BatchDatabase;
use cfd_protocol::{
    commit_descriptor, create_cfd_transactions, lock_descriptor, PartyParams, PunishParams,
    WalletExt,
};
use futures::Future;
use rust_decimal_macros::dec;
use tokio::sync::{mpsc, watch};

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Command {
    TakeOffer {
        taker_id: TakerId,
        offer_id: CfdOfferId,
        quantity: Usd,
    },
    NewOffer(CfdOffer),
    StartContractSetup {
        taker_id: TakerId,
        offer_id: CfdOfferId,
    },
    NewTakerOnline {
        id: TakerId,
    },
    IncProtocolMsg(SetupMsg),
    CfdSetupCompleted(FinalizedCfd),
}

pub fn new<B, D>(
    db: sqlx::SqlitePool,
    wallet: bdk::Wallet<B, D>,
    oracle_pk: schnorrsig::PublicKey,
    takers: mpsc::UnboundedSender<maker_inc_connections_actor::Command>,
    cfd_feed_actor_inbox: watch::Sender<Vec<Cfd>>,
    offer_feed_sender: watch::Sender<Option<CfdOffer>>,
) -> (
    impl Future<Output = ()>,
    mpsc::UnboundedSender<maker_cfd_actor::Command>,
)
where
    D: BatchDatabase,
{
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut current_contract_setup = None;

    let mut current_offer_id = None;

    let actor = {
        let sender = sender.clone();

        async move {
            // populate the CFD feed with existing CFDs
            let mut conn = db.acquire().await.unwrap();
            cfd_feed_actor_inbox
                .send(db::load_all_cfds(&mut conn).await.unwrap())
                .unwrap();

            while let Some(message) = receiver.recv().await {
                match message {
                    maker_cfd_actor::Command::TakeOffer {
                        taker_id,
                        offer_id,
                        quantity,
                    } => {
                        println!(
                            "Taker {} wants to take {} of offer {}",
                            taker_id, quantity, offer_id
                        );

                        let mut conn = db.acquire().await.unwrap();

                        // 1. Validate if offer is still valid
                        let current_offer = match current_offer_id {
                            Some(current_offer_id) if current_offer_id == offer_id => {
                                db::load_offer_by_id(current_offer_id, &mut conn)
                                    .await
                                    .unwrap()
                            }
                            _ => {
                                takers
                                .send(maker_inc_connections_actor::Command::NotifyInvalidOfferId {
                                    id: offer_id,
                                    taker_id,
                                })
                                .unwrap();
                                continue;
                            }
                        };

                        // 2. Insert CFD in DB
                        // TODO: Don't auto-accept, present to user in UI instead
                        let cfd = Cfd::new(
                            current_offer,
                            quantity,
                            CfdState::Accepted {
                                common: CfdStateCommon {
                                    transition_timestamp: SystemTime::now(),
                                },
                            },
                            Usd(dec!(10001)),
                        )
                        .unwrap();
                        db::insert_cfd(cfd, &mut conn).await.unwrap();

                        takers
                            .send(maker_inc_connections_actor::Command::NotifyOfferAccepted {
                                id: offer_id,
                                taker_id,
                            })
                            .unwrap();
                        cfd_feed_actor_inbox
                            .send(db::load_all_cfds(&mut conn).await.unwrap())
                            .unwrap();

                        // 3. Remove current offer
                        current_offer_id = None;
                        takers
                            .send(maker_inc_connections_actor::Command::BroadcastCurrentOffer(
                                None,
                            ))
                            .unwrap();
                        offer_feed_sender.send(None).unwrap();
                    }
                    maker_cfd_actor::Command::NewOffer(offer) => {
                        // 1. Save to DB
                        let mut conn = db.acquire().await.unwrap();
                        db::insert_cfd_offer(&offer, &mut conn).await.unwrap();

                        // 2. Update actor state to current offer
                        current_offer_id.replace(offer.id);

                        // 3. Notify UI via feed
                        offer_feed_sender.send(Some(offer.clone())).unwrap();

                        // 4. Inform connected takers
                        takers
                            .send(maker_inc_connections_actor::Command::BroadcastCurrentOffer(
                                Some(offer),
                            ))
                            .unwrap();
                    }
                    maker_cfd_actor::Command::NewTakerOnline { id: taker_id } => {
                        let mut conn = db.acquire().await.unwrap();

                        let current_offer = match current_offer_id {
                            Some(current_offer_id) => Some(
                                db::load_offer_by_id(current_offer_id, &mut conn)
                                    .await
                                    .unwrap(),
                            ),
                            None => None,
                        };

                        takers
                            .send(maker_inc_connections_actor::Command::SendCurrentOffer {
                                offer: current_offer,
                                taker_id,
                            })
                            .unwrap();
                    }
                    maker_cfd_actor::Command::StartContractSetup {
                        taker_id,
                        offer_id: _offer_id,
                    } => {
                        // Kick-off the CFD protocol
                        let (sk, pk) = crate::keypair::new(&mut rand::thread_rng());

                        // TODO: Load correct quantity from DB with offer_id
                        let maker_params = wallet
                            .build_party_params(bitcoin::Amount::ZERO, pk)
                            .unwrap();

                        let (actor, inbox) = setup_contract(
                            {
                                let inbox = takers.clone();
                                move |msg| {
                                    inbox
                                        .send(
                                            maker_inc_connections_actor::Command::OutProtocolMsg {
                                                taker_id,
                                                msg,
                                            },
                                        )
                                        .unwrap()
                                }
                            },
                            maker_params,
                            sk,
                            oracle_pk,
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
                    maker_cfd_actor::Command::IncProtocolMsg(msg) => {
                        let inbox = match &current_contract_setup {
                            None => panic!("whoops"),
                            Some(inbox) => inbox,
                        };

                        inbox.send(msg).unwrap();
                    }
                    maker_cfd_actor::Command::CfdSetupCompleted(_finalized_cfd) => {
                        todo!("but what?")
                    }
                }
            }
        }
    };

    (actor, sender)
}

/// Given an initial set of parameters, sets up the CFD contract with the taker.
///
/// Returns the [`FinalizedCfd`] which contains the lock transaction, ready to be signed and sent to
/// the taker. Signing of the lock transaction is not included in this function because we want the
/// actor above to own the wallet.
fn setup_contract(
    send_to_taker: impl Fn(SetupMsg),
    maker: PartyParams,
    sk: SecretKey,
    oracle_pk: schnorrsig::PublicKey,
) -> (
    impl Future<Output = FinalizedCfd>,
    mpsc::UnboundedSender<SetupMsg>,
) {
    let (sender, mut receiver) = mpsc::unbounded_channel::<SetupMsg>();

    let actor = async move {
        let (rev_sk, rev_pk) = crate::keypair::new(&mut rand::thread_rng());
        let (publish_sk, publish_pk) = crate::keypair::new(&mut rand::thread_rng());

        let maker_punish = PunishParams {
            revocation_pk: rev_pk,
            publish_pk,
        };
        send_to_taker(SetupMsg::Msg0(Msg0::from((maker.clone(), maker_punish))));

        let msg0 = receiver.recv().await.unwrap().try_into_msg0().unwrap();
        let (taker, taker_punish) = msg0.into();

        let maker_cfd_txs = create_cfd_transactions(
            (maker.clone(), maker_punish),
            (taker.clone(), taker_punish),
            oracle_pk,
            0, // TODO: Calculate refund timelock based on CFD term
            vec![],
            sk,
        )
        .unwrap();

        send_to_taker(SetupMsg::Msg1(Msg1::from(maker_cfd_txs.clone())));
        let msg1 = receiver.recv().await.unwrap().try_into_msg1().unwrap();

        let _lock_desc = lock_descriptor(taker.identity_pk, taker.identity_pk);
        // let lock_amount = maker_lock_amount + taker_lock_amount;
        let _commit_desc = commit_descriptor(
            (
                taker.identity_pk,
                taker_punish.revocation_pk,
                taker_punish.publish_pk,
            ),
            (taker.identity_pk, rev_pk, publish_pk),
        );
        let commit_tx = maker_cfd_txs.commit.0;
        let _commit_amount = Amount::from_sat(commit_tx.output[0].value);

        // TODO: Verify all signatures from the taker here

        let lock_tx = maker_cfd_txs.lock;
        let refund_tx = maker_cfd_txs.refund.0;

        let mut cet_by_id = maker_cfd_txs
            .cets
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
