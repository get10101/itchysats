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
    out_msg_maker_inbox: mpsc::UnboundedSender<wire::TakerToMaker>,
) -> (impl Future<Output = ()>, mpsc::UnboundedSender<Command>) {
    let (sender, mut receiver) = mpsc::unbounded_channel();

    let actor = async move {
        while let Some(message) = receiver.recv().await {
            match message {
                Command::TakeOffer { offer_id, quantity } => {
                    let mut conn = db.acquire().await.unwrap();

                    let current_offer = db::load_offer_by_id_from_conn(offer_id, &mut conn)
                        .await
                        .unwrap();

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
                Command::NewOffer(Some(_offer)) => todo!(),
                Command::NewOffer(None) => todo!(),
                Command::OfferAccepted(_offer_id) => {
                    todo!()
                }
            }
        }
    };

    (actor, sender)
}
