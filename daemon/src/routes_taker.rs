use crate::model::cfd::{calculate_buy_margin, Cfd, Order, OrderId};
use crate::model::{Leverage, Usd, WalletInfo};
use crate::routes::EmbeddedFileExt;
use crate::to_sse_event::{CfdsWithCurrentPrice, ToSseEvent};
use crate::{bitmex_price_feed, taker_cfd};
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
use tokio::sync::watch;
use xtra::Address;

#[rocket::get("/feed")]
pub async fn feed(
    rx_cfds: &State<watch::Receiver<Vec<Cfd>>>,
    rx_order: &State<watch::Receiver<Option<Order>>>,
    rx_wallet: &State<watch::Receiver<WalletInfo>>,
    rx_quote: &State<watch::Receiver<bitmex_price_feed::Quote>>,
) -> EventStream![] {
    let mut rx_cfds = rx_cfds.inner().clone();
    let mut rx_order = rx_order.inner().clone();
    let mut rx_wallet = rx_wallet.inner().clone();
    let mut rx_quote = rx_quote.inner().clone();

    EventStream! {
        let wallet_info = rx_wallet.borrow().clone();
        yield wallet_info.to_sse_event();

        let order = rx_order.borrow().clone();
        yield order.to_sse_event();

        let quote = rx_quote.borrow().clone();
        yield quote.to_sse_event();

        let cfds_with_price = CfdsWithCurrentPrice{cfds: rx_cfds.borrow().clone(), current_price: quote.for_taker()};
        yield cfds_with_price.to_sse_event();

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
                    let cfds_with_price = CfdsWithCurrentPrice{cfds: rx_cfds.borrow().clone(), current_price: quote.for_taker()};
                    yield cfds_with_price.to_sse_event();
                }
                Ok(()) = rx_quote.changed() => {
                    let quote = rx_quote.borrow().clone();
                    yield quote.to_sse_event();
                    let cfds_with_price = CfdsWithCurrentPrice{cfds: rx_cfds.borrow().clone(), current_price: quote.for_taker()};
                    yield cfds_with_price.to_sse_event();
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
    cfd_actor_inbox: &State<Address<taker_cfd::Actor>>,
) {
    cfd_actor_inbox
        .do_send_async(taker_cfd::TakeOffer {
            order_id: cfd_order_request.order_id,
            quantity: cfd_order_request.quantity,
        })
        .await
        .expect("actor to always be available");
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
