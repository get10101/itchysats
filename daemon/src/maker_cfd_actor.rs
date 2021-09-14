use std::time::SystemTime;

use crate::model::cfd::{Cfd, CfdOffer, CfdOfferId, CfdState, CfdStateCommon};
use crate::model::{TakerId, Usd};
use crate::{db, maker_cfd_actor, maker_inc_connections_actor};
use futures::Future;
use rust_decimal_macros::dec;
use tokio::sync::{mpsc, watch};

#[derive(Debug)]
pub enum Command {
    TakeOffer {
        taker_id: TakerId,
        offer_id: CfdOfferId,
        quantity: Usd,
    },
    NewOffer(CfdOffer),
    NewTakerOnline {
        id: TakerId,
    },
}

pub fn new(
    db: sqlx::SqlitePool,
    takers: mpsc::UnboundedSender<maker_inc_connections_actor::Command>,
    cfd_feed_actor_inbox: watch::Sender<Vec<Cfd>>,
    offer_feed_sender: watch::Sender<Option<CfdOffer>>,
) -> (
    impl Future<Output = ()>,
    mpsc::UnboundedSender<maker_cfd_actor::Command>,
) {
    let (sender, mut receiver) = mpsc::unbounded_channel();

    let mut current_offer_id = None;

    let actor = async move {
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
            }
        }
    };

    (actor, sender)
}
