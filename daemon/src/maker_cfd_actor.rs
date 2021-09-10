use crate::model::cfd::{CfdOffer, CfdOfferId};
use crate::model::Usd;
use crate::{maker_cfd_actor, maker_inc_connections_actor};
use futures::Future;
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug)]
pub enum Command {
    TakeOffer {
        taker_id: Uuid,
        offer_id: CfdOfferId,
        quantity: Usd,
    },
    NewOffer(CfdOffer),
}

pub fn new(
    _db: sqlx::SqlitePool,
    _taker_connections_inbox: mpsc::UnboundedSender<maker_inc_connections_actor::Command>,
) -> (
    impl Future<Output = ()>,
    mpsc::UnboundedSender<maker_cfd_actor::Command>,
) {
    let (sender, mut receiver) = mpsc::unbounded_channel();

    let actor = async move {
        while let Some(message) = receiver.recv().await {
            match message {
                maker_cfd_actor::Command::TakeOffer {
                    taker_id: _,
                    offer_id: _,
                    quantity: _,
                } => {

                    // println!("Received a CFD offer take request {:?}", cfd_take_request);

                    // 1. check if current offer still valid
                    // 2. make cfd and save to DB

                    // let cfd = Cfd::new(
                    //     current_offer,
                    //     cfd_take_request.quantity,
                    //     CfdState::PendingTakeRequest {
                    //         common: CfdStateCommon {
                    //             transition_timestamp: SystemTime::now(),
                    //         },
                    //     },
                    //     Usd(dec!(10001)),
                    // )
                    // .unwrap();

                    // 3. Update feed to prompt user for confirmation
                    // 4. Update other takers to remove current offer

                    // Temporarily: Auto-confirm to taker via `taker_connections_inbox`
                }
                maker_cfd_actor::Command::NewOffer(_offer) => {
                    // db_command_sender.send(Command::SaveOffer(cfd_offer.clone())).await.unwrap();

                    // offer_feed_sender.send(Some(cfd_offer.clone())).unwrap();

                    // for sender in write_connections.values() {
                    //     sender.send(Message::CurrentOffer(Some(cfd_offer.clone()))).expect("Could not communicate with taker");
                    // }
                }
            }
        }
    };

    (actor, sender)
}
