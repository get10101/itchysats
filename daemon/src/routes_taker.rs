use crate::model::cfd::{Cfd, CfdOffer, CfdTakeRequest};
use crate::taker_cfd_actor;
use crate::to_sse_event::ToSseEvent;
use bdk::bitcoin::Amount;
use rocket::response::stream::EventStream;
use rocket::serde::json::Json;
use rocket::State;
use tokio::select;
use tokio::sync::{mpsc, watch};

#[rocket::get("/feed")]
pub async fn feed(
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

#[rocket::post("/cfd", data = "<cfd_take_request>")]
pub async fn post_cfd(
    cfd_take_request: Json<CfdTakeRequest>,
    cfd_actor_inbox: &State<mpsc::UnboundedSender<taker_cfd_actor::Command>>,
) {
    cfd_actor_inbox
        .send(taker_cfd_actor::Command::TakeOffer {
            offer_id: cfd_take_request.offer_id,
            quantity: cfd_take_request.quantity,
        })
        .expect("actor to never disappear");
}

#[rocket::get("/alive")]
pub fn get_health_check() {}
