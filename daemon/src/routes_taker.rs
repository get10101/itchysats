use bdk::bitcoin::{Amount, Network};
use daemon::model::cfd::{calculate_long_margin, Cfd, Order, OrderId, Role, UpdateCfdProposals};
use daemon::model::{Leverage, Price, Usd, WalletInfo};
use daemon::routes::EmbeddedFileExt;
use daemon::to_sse_event::{CfdAction, CfdsWithAuxData, ToSseEvent};
use daemon::{bitmex_price_feed, taker_cfd};
use http_api_problem::{HttpApiProblem, StatusCode};
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
use xtra::prelude::*;

#[rocket::get("/feed")]
pub async fn feed(
    rx_cfds: &State<watch::Receiver<Vec<Cfd>>>,
    rx_order: &State<watch::Receiver<Option<Order>>>,
    rx_wallet: &State<watch::Receiver<WalletInfo>>,
    rx_quote: &State<watch::Receiver<bitmex_price_feed::Quote>>,
    rx_settlements: &State<watch::Receiver<UpdateCfdProposals>>,
    network: &State<Network>,
) -> EventStream![] {
    let mut rx_cfds = rx_cfds.inner().clone();
    let mut rx_order = rx_order.inner().clone();
    let mut rx_wallet = rx_wallet.inner().clone();
    let mut rx_quote = rx_quote.inner().clone();
    let mut rx_settlements = rx_settlements.inner().clone();
    let network = *network.inner();

    EventStream! {
        let wallet_info = rx_wallet.borrow().clone();
        yield wallet_info.to_sse_event();

        let order = rx_order.borrow().clone();
        yield order.to_sse_event();

        let quote = rx_quote.borrow().clone();
        yield quote.to_sse_event();

        yield CfdsWithAuxData::new(
            &rx_cfds,
            &rx_quote,
            &rx_settlements,
            Role::Taker,
            network
        ).to_sse_event();

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
                    yield CfdsWithAuxData::new(
                        &rx_cfds,
                        &rx_quote,
                        &rx_settlements,
                        Role::Taker,
                        network
                    ).to_sse_event();
                }
                Ok(()) = rx_settlements.changed() => {
                    yield CfdsWithAuxData::new(
                        &rx_cfds,
                        &rx_quote,
                        &rx_settlements,
                        Role::Taker,
                        network
                    ).to_sse_event();
                }
                Ok(()) = rx_quote.changed() => {
                    let quote = rx_quote.borrow().clone();
                    yield quote.to_sse_event();
                    yield CfdsWithAuxData::new(
                        &rx_cfds,
                        &rx_quote,
                        &rx_settlements,
                        Role::Taker,
                        network
                    ).to_sse_event();
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

#[rocket::post("/cfd/order", data = "<cfd_order_request>")]
pub async fn post_order_request(
    cfd_order_request: Json<CfdOrderRequest>,
    take_offer_channel: &State<Box<dyn MessageChannel<taker_cfd::TakeOffer>>>,
) -> Result<status::Accepted<()>, HttpApiProblem> {
    take_offer_channel
        .send(taker_cfd::TakeOffer {
            order_id: cfd_order_request.order_id,
            quantity: cfd_order_request.quantity,
        })
        .await
        .unwrap_or_else(|e| anyhow::bail!(e.to_string()))
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Order request failed")
                .detail(e.to_string())
        })?;

    Ok(status::Accepted(None))
}

#[rocket::post("/cfd/<id>/<action>")]
pub async fn post_cfd_action(
    id: OrderId,
    action: CfdAction,
    cfd_action_channel: &State<Box<dyn MessageChannel<taker_cfd::CfdAction>>>,
    quote_updates: &State<watch::Receiver<bitmex_price_feed::Quote>>,
) -> Result<status::Accepted<()>, HttpApiProblem> {
    use taker_cfd::CfdAction::*;
    let result = match action {
        CfdAction::AcceptOrder
        | CfdAction::RejectOrder
        | CfdAction::AcceptSettlement
        | CfdAction::RejectSettlement
        | CfdAction::AcceptRollOver
        | CfdAction::RejectRollOver => {
            return Err(HttpApiProblem::new(StatusCode::BAD_REQUEST)
                .detail(format!("taker cannot invoke action {}", action)));
        }
        CfdAction::Commit => cfd_action_channel.send(Commit { order_id: id }),
        CfdAction::Settle => {
            let current_price = quote_updates.borrow().for_taker();
            cfd_action_channel.send(ProposeSettlement {
                order_id: id,
                current_price,
            })
        }
    };

    result
        .await
        .unwrap_or_else(|e| anyhow::bail!(e.to_string()))
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title(action.to_string() + " failed")
                .detail(e.to_string())
        })?;

    Ok(status::Accepted(None))
}

#[rocket::get("/alive")]
pub fn get_health_check() {}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct MarginRequest {
    pub price: Price,
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
) -> Result<status::Accepted<Json<MarginResponse>>, HttpApiProblem> {
    let margin = calculate_long_margin(
        margin_request.price,
        margin_request.quantity,
        margin_request.leverage,
    );

    Ok(status::Accepted(Some(Json(MarginResponse { margin }))))
}

#[derive(RustEmbed)]
#[folder = "../taker-frontend/dist/taker"]
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
