use crate::maker_cfd_actor;
use crate::model::cfd::{Cfd, CfdOffer};
use crate::model::Usd;
use crate::to_sse_event::ToSseEvent;
use anyhow::Result;
use bdk::bitcoin::Amount;
use rocket::response::status;
use rocket::response::stream::EventStream;
use rocket::serde::json::Json;
use rocket::State;
use serde::Deserialize;
use tokio::select;
use tokio::sync::{mpsc, watch};

#[rocket::get("/maker-feed")]
pub async fn maker_feed(
    rx_cfds: &State<watch::Receiver<Vec<Cfd>>>,
    rx_offer: &State<watch::Receiver<Option<CfdOffer>>>,
    rx_balance: &State<watch::Receiver<Amount>>,
) -> EventStream![] {
    let mut rx_cfds = rx_cfds.inner().clone();
    let mut rx_offer = rx_offer.inner().clone();
    let mut rx_balance = rx_balance.inner().clone();

    EventStream! {
        let balance = rx_balance.borrow().clone();
        yield balance.to_sse_event();

        let offer = rx_offer.borrow().clone();
        yield offer.to_sse_event();

        let cfds = rx_cfds.borrow().clone();
        yield cfds.to_sse_event();

        loop{
            select! {
                Ok(()) = rx_balance.changed() => {
                    let balance = rx_balance.borrow().clone();
                    yield balance.to_sse_event();
                },
                Ok(()) = rx_offer.changed() => {
                    let offer = rx_offer.borrow().clone();
                    yield offer.to_sse_event();
                }
                Ok(()) = rx_cfds.changed() => {
                    let cfds = rx_cfds.borrow().clone();
                    yield cfds.to_sse_event();
                }
            }
        }
    }
}

/// The maker POSTs this to create a new CfdOffer
// TODO: Use Rocket form?
#[derive(Debug, Clone, Deserialize)]
pub struct CfdNewOfferRequest {
    pub price: Usd,
    // TODO: [post-MVP] Representation of the contract size; at the moment the contract size is
    // always 1 USD
    pub min_quantity: Usd,
    pub max_quantity: Usd,
}

#[rocket::post("/offer/sell", data = "<offer>")]
pub async fn post_sell_offer(
    offer: Json<CfdNewOfferRequest>,
    cfd_actor_inbox: &State<mpsc::UnboundedSender<maker_cfd_actor::Command>>,
) -> Result<status::Accepted<()>, status::BadRequest<String>> {
    let offer = CfdOffer::from_default_with_price(offer.price)
        .map_err(|e| status::BadRequest(Some(e.to_string())))?
        .with_min_quantity(offer.min_quantity)
        .with_max_quantity(offer.max_quantity);

    cfd_actor_inbox
        .send(maker_cfd_actor::Command::NewOffer(offer))
        .expect("actor to always be available");

    Ok(status::Accepted(None))
}

// // TODO: Shall we use a simpler struct for verification? AFAICT quantity is not
// // needed, no need to send the whole CFD either as the other fields can be generated from the
// offer #[rocket::post("/offer/confirm", data = "<cfd_confirm_offer_request>")]
// pub async fn post_confirm_offer(
//     cfd_confirm_offer_request: Json<CfdTakeRequest>,
//     queue: &State<mpsc::Sender<CfdOffer>>,
//     mut conn: Connection<Db>,
// ) -> Result<status::Accepted<()>, status::BadRequest<String>> {
//     dbg!(&cfd_confirm_offer_request);

//     let offer = db::load_offer_by_id_from_conn(cfd_confirm_offer_request.offer_id, &mut conn)
//         .await
//         .map_err(|e| status::BadRequest(Some(e.to_string())))?;

//     let _res = queue
//         .send(offer)
//         .await
//         .map_err(|_| status::BadRequest(Some("internal server error".to_string())))?;

//     Ok(status::Accepted(None))
// }

#[rocket::get("/alive")]
pub fn get_health_check() {}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RetrieveCurrentOffer;
