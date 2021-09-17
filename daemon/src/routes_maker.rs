use crate::maker_cfd_actor;
use crate::model::cfd::{Cfd, Order, Origin};
use crate::model::{Usd, WalletInfo};
use crate::to_sse_event::ToSseEvent;
use anyhow::Result;

use rocket::response::status;
use rocket::response::stream::EventStream;
use rocket::serde::json::Json;
use rocket::State;
use serde::Deserialize;
use tokio::select;
use tokio::sync::{mpsc, watch};

#[rocket::get("/feed")]
pub async fn maker_feed(
    rx_cfds: &State<watch::Receiver<Vec<Cfd>>>,
    rx_order: &State<watch::Receiver<Option<Order>>>,
    rx_wallet: &State<watch::Receiver<WalletInfo>>,
) -> EventStream![] {
    let mut rx_cfds = rx_cfds.inner().clone();
    let mut rx_order = rx_order.inner().clone();
    let mut rx_wallet = rx_wallet.inner().clone();

    EventStream! {
        let wallet_info = rx_wallet.borrow().clone();
        yield wallet_info.to_sse_event();

        let order = rx_order.borrow().clone();
        yield order.to_sse_event();

        let cfds = rx_cfds.borrow().clone();
        yield cfds.to_sse_event();

        loop{
            select! {
                Ok(()) = rx_wallet.changed() => {
                    let wallet_info = rx_wallet.borrow().clone();
                    yield wallet_info.to_sse_event();
                },
                Ok(()) = rx_order.changed() => {
                    let order = rx_order.borrow().clone();
                    yield order.to_sse_event();
                }
                Ok(()) = rx_cfds.changed() => {
                    let cfds = rx_cfds.borrow().clone();
                    yield cfds.to_sse_event();
                }
            }
        }
    }
}

/// The maker POSTs this to create a new CfdOrder
// TODO: Use Rocket form?
#[derive(Debug, Clone, Deserialize)]
pub struct CfdNewOrderRequest {
    pub price: Usd,
    // TODO: [post-MVP] Representation of the contract size; at the moment the contract size is
    // always 1 USD
    pub min_quantity: Usd,
    pub max_quantity: Usd,
}

#[rocket::post("/order/sell", data = "<order>")]
pub async fn post_sell_order(
    order: Json<CfdNewOrderRequest>,
    cfd_actor_inbox: &State<mpsc::UnboundedSender<maker_cfd_actor::Command>>,
) -> Result<status::Accepted<()>, status::BadRequest<String>> {
    let order = Order::from_default_with_price(order.price, Origin::Ours)
        .map_err(|e| status::BadRequest(Some(e.to_string())))?
        .with_min_quantity(order.min_quantity)
        .with_max_quantity(order.max_quantity);

    cfd_actor_inbox
        .send(maker_cfd_actor::Command::NewOrder(order))
        .expect("actor to always be available");

    Ok(status::Accepted(None))
}

// // TODO: Shall we use a simpler struct for verification? AFAICT quantity is not
// // needed, no need to send the whole CFD either as the other fields can be generated from the
// order #[rocket::post("/order/confirm", data = "<cfd_confirm_order_request>")]
// pub async fn post_confirm_order(
//     cfd_confirm_order_request: Json<CfdTakeRequest>,
//     queue: &State<mpsc::Sender<CfdOrder>>,
//     mut conn: Connection<Db>,
// ) -> Result<status::Accepted<()>, status::BadRequest<String>> {
//     dbg!(&cfd_confirm_order_request);

//     let order = db::load_order_by_id_from_conn(cfd_confirm_order_request.order_id, &mut conn)
//         .await
//         .map_err(|e| status::BadRequest(Some(e.to_string())))?;

//     let _res = queue
//         .send(order)
//         .await
//         .map_err(|_| status::BadRequest(Some("internal server error".to_string())))?;

//     Ok(status::Accepted(None))
// }

#[rocket::get("/alive")]
pub fn get_health_check() {}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RetrieveCurrentOrder;
