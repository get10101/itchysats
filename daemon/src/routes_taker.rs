use crate::model::cfd::{calculate_buy_margin, Cfd, Order, OrderId};
use crate::model::{Leverage, Usd, WalletInfo};
use crate::routes::EmbeddedFileExt;
use crate::taker_cfd_actor;
use crate::to_sse_event::ToSseEvent;
use bdk::bitcoin::Amount;
use rocket::http::{ContentType, Status};
use rocket::response::stream::EventStream;
use rocket::response::{status, Responder};
use rocket::serde::json::Json;
use rocket::State;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::PathBuf;
use tokio::select;
use tokio::sync::{mpsc, watch};

#[rocket::get("/feed")]
pub async fn feed(
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CfdOrderRequest {
    pub order_id: OrderId,
    pub quantity: Usd,
}

#[rocket::post("/cfd", data = "<cfd_order_request>")]
pub async fn post_order_request(
    cfd_order_request: Json<CfdOrderRequest>,
    cfd_actor_inbox: &State<mpsc::UnboundedSender<taker_cfd_actor::Command>>,
) {
    cfd_actor_inbox
        .send(taker_cfd_actor::Command::TakeOffer {
            order_id: cfd_order_request.order_id,
            quantity: cfd_order_request.quantity,
        })
        .expect("actor to never disappear");
}

#[rocket::get("/alive")]
pub fn get_health_check() {}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct MarginRequest {
    pub price: Usd,
    pub quantity: Usd,
    pub leverage: Leverage,
}

/// Represents the collateral that has to be put up
#[derive(Debug, Clone, Copy, Serialize)]
pub struct MarginResponse {
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub margin: Amount,
}

// TODO: Consider moving this into wasm and load it into the UI instead of triggering this endpoint
// upon every quantity keystroke
#[rocket::post("/calculate/margin", data = "<margin_request>")]
pub fn margin_calc(
    margin_request: Json<MarginRequest>,
) -> Result<status::Accepted<Json<MarginResponse>>, status::BadRequest<String>> {
    let margin = calculate_buy_margin(
        margin_request.price,
        margin_request.quantity,
        margin_request.leverage,
    )
    .map_err(|e| status::BadRequest(Some(e.to_string())))?;

    Ok(status::Accepted(Some(Json(MarginResponse { margin }))))
}

#[derive(RustEmbed)]
#[folder = "../frontend/dist/maker"]
struct Asset;

#[rocket::get("/assets/<file..>")]
pub fn dist<'r>(file: PathBuf) -> impl Responder<'r, 'static> {
    let filename = format!("assets/{}", file.display().to_string());
    Asset::get(&filename).into_response(file)
}

#[rocket::get("/<_paths..>", format = "text/html")]
pub fn index<'r>(_paths: PathBuf) -> impl Responder<'r, 'static> {
    let asset = Asset::get("index.html").ok_or(Status::NotFound)?;
    Ok::<(ContentType, Cow<[u8]>), Status>((ContentType::HTML, asset.data))
}
