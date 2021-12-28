use bdk::bitcoin::{Amount, Network};
use daemon::auth::Authenticated;
use daemon::connection::ConnectionStatus;
use daemon::model::cfd::{calculate_long_margin, OrderId};
use daemon::model::{Leverage, Price, Usd, WalletInfo};
use daemon::projection::{CfdAction, Feeds};
use daemon::routes::EmbeddedFileExt;
use daemon::to_sse_event::ToSseEvent;
use daemon::{bitmex_price_feed, monitor, oracle, taker_cfd, tx, wallet};
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

type Taker = xtra::Address<taker_cfd::Actor<oracle::Actor, monitor::Actor, wallet::Actor>>;

#[rocket::get("/feed")]
pub async fn feed(
    rx: &State<Feeds>,
    rx_wallet: &State<watch::Receiver<Option<WalletInfo>>>,
    rx_maker_status: &State<watch::Receiver<ConnectionStatus>>,
    _auth: Authenticated,
) -> EventStream![] {
    let rx = rx.inner();
    let mut rx_cfds = rx.cfds.clone();
    let mut rx_order = rx.order.clone();
    let mut rx_quote = rx.quote.clone();
    let mut rx_wallet = rx_wallet.inner().clone();
    let mut rx_maker_status = rx_maker_status.inner().clone();

    EventStream! {
        let wallet_info = rx_wallet.borrow().clone();
        yield wallet_info.to_sse_event();

        let maker_status = rx_maker_status.borrow().clone();
        yield maker_status.to_sse_event();

        let order = rx_order.borrow().clone();
        yield order.to_sse_event();

        let quote = rx_quote.borrow().clone();
        yield quote.to_sse_event();

        let cfds = rx_cfds.borrow().clone();
        yield cfds.to_sse_event();

        loop{
            select! {
                Ok(()) = rx_wallet.changed() => {
                    let wallet_info = rx_wallet.borrow().clone();
                    yield wallet_info.to_sse_event();
                },
                Ok(()) = rx_maker_status.changed() => {
                    let maker_status = rx_maker_status.borrow().clone();
                    yield maker_status.to_sse_event();
                },
                Ok(()) = rx_order.changed() => {
                    let order = rx_order.borrow().clone();
                    yield order.to_sse_event();
                }
                Ok(()) = rx_cfds.changed() => {
                    let cfds = rx_cfds.borrow().clone();
                    yield cfds.to_sse_event();
                }
                Ok(()) = rx_quote.changed() => {
                    let quote = rx_quote.borrow().clone();
                    yield quote.to_sse_event();
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
    cfd_actor: &State<Taker>,
    _auth: Authenticated,
) -> Result<status::Accepted<()>, HttpApiProblem> {
    cfd_actor
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
    cfd_actor: &State<Taker>,
    feeds: &State<Feeds>,
    _auth: Authenticated,
) -> Result<status::Accepted<()>, HttpApiProblem> {
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
        CfdAction::Commit => cfd_actor.send(taker_cfd::Commit { order_id: id }).await,
        CfdAction::Settle => {
            let quote: bitmex_price_feed::Quote = feeds.quote.borrow().clone().into();
            let current_price = quote.for_taker();
            cfd_actor
                .send(taker_cfd::ProposeSettlement {
                    order_id: id,
                    current_price,
                })
                .await
        }
        CfdAction::RollOver => {
            cfd_actor
                .send(taker_cfd::ProposeRollOver { order_id: id })
                .await
        }
    };

    result
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
    _auth: Authenticated,
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
pub fn dist<'r>(file: PathBuf, _auth: Authenticated) -> impl Responder<'r, 'static> {
    let filename = format!("assets/{}", file.display().to_string());
    Asset::get(&filename).into_response(file)
}

#[rocket::get("/<_paths..>", format = "text/html")]
pub fn index<'r>(_paths: PathBuf, _auth: Authenticated) -> impl Responder<'r, 'static> {
    let asset = Asset::get("index.html").ok_or(Status::NotFound)?;
    Ok::<(ContentType, Cow<[u8]>), Status>((ContentType::HTML, asset.data))
}

#[derive(Debug, Clone, Deserialize)]
pub struct WithdrawRequest {
    address: bdk::bitcoin::Address,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    amount: Amount,
    fee: f32,
}

#[rocket::post("/withdraw", data = "<withdraw_request>")]
pub async fn post_withdraw_request(
    withdraw_request: Json<WithdrawRequest>,
    wallet: &State<Address<wallet::Actor>>,
    network: &State<Network>,
    _auth: Authenticated,
) -> Result<String, HttpApiProblem> {
    let amount =
        (withdraw_request.amount != bdk::bitcoin::Amount::ZERO).then(|| withdraw_request.amount);

    let txid = wallet
        .send(wallet::Withdraw {
            amount,
            address: withdraw_request.address.clone(),
            fee: Some(bdk::FeeRate::from_sat_per_vb(withdraw_request.fee)),
        })
        .await
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Could not proceed with withdraw request")
                .detail(e.to_string())
        })?
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::BAD_REQUEST)
                .title("Could not withdraw funds")
                .detail(e.to_string())
        })?;

    Ok(tx::to_mempool_url(txid, *network.inner()))
}
