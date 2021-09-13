use crate::model::cfd::{Cfd, CfdOffer, CfdOfferId, CfdState, CfdStateCommon};
use crate::model::Usd;
use crate::{db, wire};
use futures::Future;
use std::time::SystemTime;
use tokio::sync::{mpsc, watch};

#[derive(Debug)]
pub enum Command {
    TakeOffer { offer_id: CfdOfferId, quantity: Usd },
    NewOffer(Option<CfdOffer>),
    OfferAccepted(CfdOfferId),
}

pub fn new(
    db: sqlx::SqlitePool,
    cfd_feed_actor_inbox: watch::Sender<Vec<Cfd>>,
    offer_feed_actor_inbox: watch::Sender<Option<CfdOffer>>,
    out_msg_maker_inbox: mpsc::UnboundedSender<wire::TakerToMaker>,
) -> (impl Future<Output = ()>, mpsc::UnboundedSender<Command>) {
    let (sender, mut receiver) = mpsc::unbounded_channel();

    let actor = async move {
        while let Some(message) = receiver.recv().await {
            match message {
                Command::TakeOffer { offer_id, quantity } => {
                    let mut conn = db.acquire().await.unwrap();

                    let current_offer = db::load_offer_by_id(offer_id, &mut conn).await.unwrap();

                    println!("Accepting current offer: {:?}", &current_offer);

                    let cfd = Cfd::new(
                        current_offer,
                        quantity,
                        CfdState::PendingTakeRequest {
                            common: CfdStateCommon {
                                transition_timestamp: SystemTime::now(),
                            },
                        },
                        Usd::ZERO,
                    )
                    .unwrap();

                    db::insert_cfd(cfd, &mut conn).await.unwrap();

                    cfd_feed_actor_inbox
                        .send(db::load_all_cfds(&mut conn).await.unwrap())
                        .unwrap();
                    out_msg_maker_inbox
                        .send(wire::TakerToMaker::TakeOffer { offer_id, quantity })
                        .unwrap();
                }
                Command::NewOffer(Some(offer)) => {
                    let mut conn = db.acquire().await.unwrap();
                    db::insert_cfd_offer(&offer, &mut conn).await.unwrap();
                    offer_feed_actor_inbox.send(Some(offer)).unwrap();
                }

                Command::NewOffer(None) => {
                    offer_feed_actor_inbox.send(None).unwrap();
                }

                Command::OfferAccepted(offer_id) => {
                    let mut conn = db.acquire().await.unwrap();
                    db::insert_new_cfd_state_by_offer_id(
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
                        .send(db::load_all_cfds(&mut conn).await.unwrap())
                        .unwrap();

                    // TODO: Contract signing/setup
                }
            }
        }
    };

    (actor, sender)
}
