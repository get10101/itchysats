use anyhow::Result;
use daemon::bdk;
use daemon::bdk::bitcoin::Network;
use daemon::bdk::blockchain::ElectrumBlockchain;
use daemon::oracle;
use daemon::projection::Cfd;
use daemon::projection::CfdAction;
use daemon::projection::Feeds;
use daemon::wallet;
use daemon::MakerActorSystem;
use http_api_problem::HttpApiProblem;
use http_api_problem::StatusCode;
use model::FundingRate;
use model::Identity;
use model::OpeningFee;
use model::OrderId;
use model::Price;
use model::TxFeeRate;
use model::Usd;
use model::WalletInfo;
use rocket::http::ContentType;
use rocket::http::Status;
use rocket::response::stream::Event;
use rocket::response::stream::EventStream;
use rocket::response::Responder;
use rocket::serde::json::Json;
use rocket::State;
use rocket_basicauth::Authenticated;
use rust_embed::RustEmbed;
use rust_embed_rocket::EmbeddedFileExt;
use serde::Deserialize;
use shared_bin::ToSseEvent;
use std::borrow::Cow;
use std::path::PathBuf;
use tokio::select;
use tokio::sync::watch;
use uuid::Uuid;

pub type Maker = MakerActorSystem<oracle::Actor, wallet::Actor<ElectrumBlockchain>>;

#[allow(clippy::too_many_arguments)]
#[rocket::get("/feed")]
pub async fn maker_feed(
    rx: &State<Feeds>,
    rx_wallet: &State<watch::Receiver<Option<WalletInfo>>>,
    _auth: Authenticated,
) -> EventStream![] {
    let rx = rx.inner();
    let mut rx_cfds = rx.cfds.clone();
    let mut rx_offers = rx.offers.clone();
    let mut rx_wallet = rx_wallet.inner().clone();
    let mut rx_quote = rx.quote.clone();
    let mut rx_connected_takers = rx.connected_takers.clone();

    EventStream! {
        let wallet_info = rx_wallet.borrow().clone();
        yield wallet_info.to_sse_event();

        let offers = rx_offers.borrow().clone();
        yield Event::json(&offers.long).event("long_offer");
        yield Event::json(&offers.short).event("short_offer");

        let quote = rx_quote.borrow().clone();
        yield quote.to_sse_event();

        let cfds = rx_cfds.borrow().clone();
        if let Some(cfds) = cfds {
            yield cfds.to_sse_event()
        }

        let takers = rx_connected_takers.borrow().clone();
        yield takers.to_sse_event();

        loop{
            select! {
                Ok(()) = rx_wallet.changed() => {
                    let wallet_info = rx_wallet.borrow().clone();
                    yield wallet_info.to_sse_event();
                },
                Ok(()) = rx_offers.changed() => {
                    let offers = rx_offers.borrow().clone();
                    yield Event::json(&offers.long).event("long_offer");
                    yield Event::json(&offers.short).event("short_offer");
                }
                Ok(()) = rx_connected_takers.changed() => {
                    let takers = rx_connected_takers.borrow().clone();
                    yield takers.to_sse_event();
                }
                Ok(()) = rx_cfds.changed() => {
                    let cfds = rx_cfds.borrow().clone();
                    if let Some(cfds) = cfds {
                        yield cfds.to_sse_event()
                    }
                }
                Ok(()) = rx_quote.changed() => {
                    let quote = rx_quote.borrow().clone();
                    yield quote.to_sse_event();
                }
            }
        }
    }
}

/// The maker PUTs this to set the offer params
#[derive(Debug, Clone, Deserialize)]
pub struct CfdNewOfferParamsRequest {
    pub price_long: Option<Price>,
    pub price_short: Option<Price>,
    pub min_quantity: Usd,
    pub max_quantity: Usd,
    /// The current _daily_ funding rate for the maker's long position
    pub daily_funding_rate_long: FundingRate,
    /// The current _daily_ funding rate for the maker's short position
    pub daily_funding_rate_short: FundingRate,
    pub tx_fee_rate: TxFeeRate,
    // TODO: This is not inline with other parts of the API! We should not expose internal types
    // here. We have to specify sats for here because of that.
    pub opening_fee: OpeningFee,
}

#[rocket::put("/offer", data = "<offer_params>")]
pub async fn put_offer_params(
    offer_params: Json<CfdNewOfferParamsRequest>,
    maker: &State<Maker>,
    _auth: Authenticated,
) -> Result<(), HttpApiProblem> {
    if offer_params.tx_fee_rate.to_u32() < 1 {
        return Err(HttpApiProblem::new(StatusCode::BAD_REQUEST)
            .title("Posting offer failed")
            .detail("TxFeeRate is below min_relay_fee (1)"));
    };

    maker
        .set_offer_params(
            offer_params.price_long,
            offer_params.price_short,
            offer_params.min_quantity,
            offer_params.max_quantity,
            offer_params.tx_fee_rate,
            offer_params.daily_funding_rate_long,
            offer_params.daily_funding_rate_short,
            offer_params.opening_fee,
        )
        .await
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Posting offer failed")
                .detail(format!("{e:#}"))
        })?;

    Ok(())
}

#[rocket::post("/cfd/<id>/<action>")]
pub async fn post_cfd_action(
    id: Uuid,
    action: String,
    maker: &State<Maker>,
    _auth: Authenticated,
) -> Result<(), HttpApiProblem> {
    let id = OrderId::from(id);
    let action = action.parse().map_err(|_| {
        HttpApiProblem::new(StatusCode::BAD_REQUEST).detail(format!("Invalid action: {}", action))
    })?;

    let result = match action {
        CfdAction::AcceptOrder => maker.accept_order(id).await,
        CfdAction::RejectOrder => maker.reject_order(id).await,
        CfdAction::AcceptSettlement => maker.accept_settlement(id).await,
        CfdAction::RejectSettlement => maker.reject_settlement(id).await,
        CfdAction::AcceptRollover => maker.accept_rollover(id).await,
        CfdAction::RejectRollover => maker.reject_rollover(id).await,
        CfdAction::Commit => maker.commit(id).await,
        CfdAction::Settle => {
            let msg = "Collaborative settlement can only be triggered by taker";
            tracing::error!(msg);
            return Err(HttpApiProblem::new(StatusCode::BAD_REQUEST).detail(msg));
        }
    };

    result.map_err(|e| {
        tracing::warn!(order_id=%id, %action, "Processing action failed: {e:#}");

        HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title(action.to_string() + " failed")
            .detail(format!("{e:#}"))
    })?;

    Ok(())
}

#[rocket::get("/alive")]
pub fn get_health_check() {}

#[derive(RustEmbed)]
#[folder = "../maker-frontend/dist/maker"]
struct Asset;

#[rocket::get("/assets/<file..>")]
pub fn dist<'r>(file: PathBuf, _auth: Authenticated) -> impl Responder<'r, 'static> {
    let filename = format!("assets/{}", file.display());
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
    #[serde(with = "bdk::bitcoin::util::amount::serde::as_btc")]
    amount: bdk::bitcoin::Amount,
    fee: f32,
}

#[rocket::post("/withdraw", data = "<withdraw_request>")]
pub async fn post_withdraw_request(
    withdraw_request: Json<WithdrawRequest>,
    maker: &State<Maker>,
    network: &State<Network>,
    _auth: Authenticated,
) -> Result<String, HttpApiProblem> {
    let amount =
        (withdraw_request.amount != bdk::bitcoin::Amount::ZERO).then(|| withdraw_request.amount);

    let txid = maker
        .withdraw(
            amount,
            withdraw_request.address.clone(),
            withdraw_request.fee,
        )
        .await
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Could not proceed with withdraw request")
                .detail(format!("{e:#}"))
        })?;

    let url = match network.inner() {
        Network::Bitcoin => format!("https://mempool.space/tx/{txid}"),
        Network::Testnet => format!("https://mempool.space/testnet/tx/{txid}"),
        Network::Signet => format!("https://mempool.space/signet/tx/{txid}"),
        Network::Regtest => txid.to_string(),
    };

    Ok(url)
}

#[rocket::get("/cfds")]
pub async fn get_cfds<'r>(
    rx: &State<Feeds>,
    _auth: Authenticated,
) -> Result<Json<Vec<Cfd>>, HttpApiProblem> {
    let rx = rx.inner();
    let rx_cfds = rx.cfds.clone();
    let cfds = rx_cfds.borrow().clone();

    match cfds {
        Some(cfds) => Ok(Json(cfds)),
        None => Err(HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("CFDs not yet available")
            .detail("CFDs are still being loaded from the database. Please retry later.")),
    }
}

#[rocket::get("/takers")]
pub async fn get_takers<'r>(
    rx: &State<Feeds>,
    _auth: Authenticated,
) -> Result<Json<Vec<Identity>>, HttpApiProblem> {
    let rx = rx.inner();
    let rx_connected_takers = rx.connected_takers.clone();
    let takers = rx_connected_takers.borrow().clone();

    Ok(Json(takers))
}

#[rocket::get("/metrics")]
pub async fn get_metrics<'r>(_auth: Authenticated) -> Result<String, HttpApiProblem> {
    let metrics = prometheus::TextEncoder::new()
        .encode_to_string(&prometheus::gather())
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to encode metrics")
                .detail(e.to_string())
        })?;

    Ok(metrics)
}
