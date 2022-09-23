#![allow(clippy::let_unit_value)] // see: https://github.com/SergioBenitez/Rocket/issues/2211
use crate::actor_system::ActorSystem;
use anyhow::Result;
use bdk::sled;
use daemon::bdk::blockchain::ElectrumBlockchain;
use daemon::oracle;
use daemon::projection::Cfd;
use daemon::projection::CfdAction;
use daemon::projection::FeedReceivers;
use daemon::wallet;
use http_api_problem::HttpApiProblem;
use http_api_problem::StatusCode;
use model::Contracts;
use model::FundingRate;
use model::Leverage;
use model::LotSize;
use model::OpeningFee;
use model::OrderId;
use model::Price;
use model::TxFeeRate;
use model::WalletInfo;
use rocket::form::Form;
use rocket::http::ContentType;
use rocket::http::Status;
use rocket::request::FromParam;
use rocket::response::stream::Event;
use rocket::response::stream::EventStream;
use rocket::response::Responder;
use rocket::serde::json::Json;
use rocket::State;
use rocket_cookie_auth::auth::Auth;
use rocket_cookie_auth::forms::ChangePassword;
use rocket_cookie_auth::forms::Login;
use rocket_cookie_auth::user::User;
use rust_embed::RustEmbed;
use rust_embed_rocket::EmbeddedFileExt;
use serde::Deserialize;
use serde::Serialize;
use shared_bin::ToSseEvent;
use std::borrow::Cow;
use std::path::PathBuf;
use tokio::select;
use tokio::sync::watch;
use tracing::instrument;
use uuid::Uuid;

pub type Maker = ActorSystem<oracle::Actor, wallet::Actor<ElectrumBlockchain, sled::Tree>>;

#[allow(clippy::too_many_arguments)]
#[rocket::get("/feed")]
#[instrument(name = "GET /feed", skip_all)]
pub async fn maker_feed(
    rx: &State<FeedReceivers>,
    rx_wallet: &State<watch::Receiver<Option<WalletInfo>>>,
    _user: User,
) -> EventStream![] {
    let rx = rx.inner();
    let mut rx_cfds = rx.cfds.clone();
    let mut rx_wallet = rx_wallet.inner().clone();
    let mut rx_offers = rx.offers.clone();
    let mut rx_quote = rx.quote.clone();

    EventStream! {
        let wallet_info = rx_wallet.borrow().clone();
        yield wallet_info.to_sse_event();

        let offers = rx_offers.borrow().clone();
        yield Event::json(&offers.btcusd_long).event("btcusd_long_offer");
        yield Event::json(&offers.btcusd_short).event("btcusd_short_offer");
        yield Event::json(&offers.ethusd_long).event("ethusd_long_offer");
        yield Event::json(&offers.ethusd_short).event("ethusd_short_offer");

        let quote = rx_quote.borrow().clone();
        yield Event::json(&quote.get(&model::ContractSymbol::BtcUsd)).event("btcusd_quote");
        yield Event::json(&quote.get(&model::ContractSymbol::EthUsd)).event("ethusd_quote");

        let cfds = rx_cfds.borrow().clone();
        if let Some(cfds) = cfds {
            yield cfds.to_sse_event()
        }

        loop{
            select! {
                Ok(()) = rx_wallet.changed() => {
                    let wallet_info = rx_wallet.borrow().clone();
                    yield wallet_info.to_sse_event();
                },
                Ok(()) = rx_offers.changed() => {
                    let offers = rx_offers.borrow().clone();
                    yield Event::json(&offers.btcusd_long).event("btcusd_long_offer");
                    yield Event::json(&offers.btcusd_short).event("btcusd_short_offer");
                    yield Event::json(&offers.ethusd_long).event("ethusd_long_offer");
                    yield Event::json(&offers.ethusd_short).event("ethusd_short_offer");
                }
                Ok(()) = rx_cfds.changed() => {
                    let cfds = rx_cfds.borrow().clone();
                    if let Some(cfds) = cfds {
                        yield cfds.to_sse_event()
                    }
                }
                Ok(()) = rx_quote.changed() => {
                    let quote = rx_quote.borrow().clone();
                    yield Event::json(&quote.get(&model::ContractSymbol::BtcUsd)).event("btcusd_quote");
                    yield Event::json(&quote.get(&model::ContractSymbol::EthUsd)).event("ethusd_quote");
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
    pub min_quantity: Contracts,
    pub max_quantity: Contracts,
    /// The current _daily_ funding rate for the maker's long position
    pub daily_funding_rate_long: FundingRate,
    /// The current _daily_ funding rate for the maker's short position
    pub daily_funding_rate_short: FundingRate,
    pub tx_fee_rate: TxFeeRate,
    // TODO: This is not inline with other parts of the API! We should not expose internal types
    // here. We have to specify sats for here because of that.
    pub opening_fee: OpeningFee,
    #[serde(default = "empty_leverage")]
    pub leverage_choices: Vec<Leverage>,
    #[serde(default = "default_lot_size")]
    pub lot_size: LotSize,
}

fn empty_leverage() -> Vec<Leverage> {
    vec![Leverage::TWO]
}

// TODO: we can remove this once all clients have been updated
fn default_lot_size() -> LotSize {
    LotSize::new(100)
}

#[rocket::put("/offer", data = "<offer_params>")]
#[instrument(name = "PUT /offer", skip(maker, _user), err)]
pub async fn put_offer_params(
    offer_params: Json<CfdNewOfferParamsRequest>,
    maker: &State<Maker>,
    _user: User,
) -> Result<(), HttpApiProblem> {
    tracing::warn!("Deprecated /offer was called. Please use /<contract_symbol>/offer from now.");
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
            offer_params.leverage_choices.clone(),
            ContractSymbol::BtcUsd.into(),
            offer_params.lot_size,
        )
        .await
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Posting offer failed")
                .detail(format!("{e:#}"))
        })?;

    Ok(())
}

#[derive(Debug, Copy, Clone, strum_macros::Display)]
pub enum ContractSymbol {
    BtcUsd,
    EthUsd,
}

impl From<ContractSymbol> for model::ContractSymbol {
    fn from(symbol: ContractSymbol) -> Self {
        match symbol {
            ContractSymbol::BtcUsd => model::ContractSymbol::BtcUsd,
            ContractSymbol::EthUsd => model::ContractSymbol::EthUsd,
        }
    }
}

impl<'r> FromParam<'r> for ContractSymbol {
    type Error = anyhow::Error;

    fn from_param(param: &'r str) -> Result<Self, Self::Error> {
        match param.to_lowercase().as_str() {
            "btcusd" => Ok(ContractSymbol::BtcUsd),
            "ethusd" => Ok(ContractSymbol::EthUsd),
            _ => anyhow::bail!("Unknown contract symbol provided: {param}"),
        }
    }
}

#[rocket::put("/<symbol>/offer", data = "<offer_params>")]
#[instrument(name = "PUT /offer", skip(maker, _user), err)]
pub async fn put_offer_params_for_symbol(
    symbol: Result<ContractSymbol>,
    offer_params: Json<CfdNewOfferParamsRequest>,
    maker: &State<Maker>,
    _user: User,
) -> Result<(), HttpApiProblem> {
    // if we use `ContractSymbol` as arg directly the error gets lost. So we need to do this:
    let symbol = symbol.map_err(|e| {
        HttpApiProblem::new(StatusCode::BAD_REQUEST)
            .title("Unknown ContractSymbol provided")
            .detail(format!("{e:#}"))
    })?;
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
            offer_params.leverage_choices.clone(),
            symbol.into(),
            offer_params.lot_size,
        )
        .await
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Posting offer failed")
                .detail(format!("{e:#}"))
        })?;

    Ok(())
}

#[rocket::post("/cfd/<order_id>/<action>")]
#[instrument(name = "POST /cfd/<order_id>/<action>", skip(maker, _user), err)]
pub async fn post_cfd_action(
    order_id: Uuid,
    action: String,
    maker: &State<Maker>,
    _user: User,
) -> Result<(), HttpApiProblem> {
    let order_id = OrderId::from(order_id);
    let action = action.parse().map_err(|_| {
        HttpApiProblem::new(StatusCode::BAD_REQUEST).detail(format!("Invalid action: {}", action))
    })?;

    let result = match action {
        CfdAction::AcceptOrder => maker.accept_order(order_id).await,
        CfdAction::RejectOrder => maker.reject_order(order_id).await,
        CfdAction::AcceptSettlement => maker.accept_settlement(order_id).await,
        CfdAction::RejectSettlement => maker.reject_settlement(order_id).await,
        CfdAction::Commit => maker.commit(order_id).await,
        CfdAction::Settle => {
            return Err(HttpApiProblem::new(StatusCode::BAD_REQUEST)
                .detail("Collaborative settlement can only be triggered by taker"));
        }
    };

    result.map_err(|e| {
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
#[instrument(name = "GET /assets/<file>", skip_all)]
pub fn dist<'r>(file: PathBuf) -> impl Responder<'r, 'static> {
    let filename = format!("assets/{}", file.display());
    Asset::get(&filename).into_response(file)
}

#[rocket::get("/<_paths..>", format = "text/html")]
#[instrument(name = "GET /<_paths>", skip_all)]
pub fn index<'r>(_paths: PathBuf) -> impl Responder<'r, 'static> {
    let asset = Asset::get("index.html").ok_or(Status::NotFound)?;
    Ok::<(ContentType, Cow<[u8]>), Status>((ContentType::HTML, asset.data))
}

#[rocket::put("/sync")]
#[instrument(name = "PUT /sync", skip_all, err)]
pub async fn put_sync_wallet(maker: &State<Maker>, _user: User) -> Result<(), HttpApiProblem> {
    maker.sync_wallet().await.map_err(|e| {
        HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Could not sync wallet")
            .detail(format!("{e:#}"))
    })?;

    Ok(())
}

#[rocket::get("/cfds")]
#[instrument(name = "GET /cfds", skip_all, err)]
pub async fn get_cfds<'r>(
    rx: &State<FeedReceivers>,
    _user: User,
) -> Result<Json<Vec<Cfd>>, HttpApiProblem> {
    let rx = rx.inner();
    let rx_cfds = rx.cfds.clone();
    let cfds = rx_cfds.borrow().clone();

    match cfds {
        Some(cfds) => Ok(Json(cfds)),
        None => Err(HttpApiProblem::new(StatusCode::SERVICE_UNAVAILABLE)
            .title("CFDs not yet available")
            .detail("CFDs are still being loaded from the database. Please retry later.")),
    }
}

// TODO: Use non-cookie auth for /metrics endpoint as Prometheus does not
// support cookie-auth (for now, leave unauthenticated)
#[rocket::get("/metrics")]
#[instrument(name = "GET /metrics", skip_all, err)]
pub async fn get_metrics<'r>() -> Result<String, HttpApiProblem> {
    let metrics = prometheus::TextEncoder::new()
        .encode_to_string(&prometheus::gather())
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to encode metrics")
                .detail(e.to_string())
        })?;

    Ok(metrics)
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct RolloverConfig {
    is_accepting_rollovers: bool,
}

#[rocket::post("/rollover/config", data = "<config>")]
#[instrument(name = "POST /rollover/config", skip(maker), err)]
pub async fn update_rollover_configuration(
    config: Json<RolloverConfig>,
    maker: &State<Maker>,
    _user: User,
) -> Result<(), HttpApiProblem> {
    maker
        .update_rollover_configuration(config.is_accepting_rollovers)
        .await
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Updating rollover configuration failed")
                .detail(format!("{e:#}"))
        })?;

    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthCheck {
    daemon_version: String,
}

#[rocket::get("/version")]
#[instrument(name = "GET /version")]
pub async fn get_version() -> Json<HealthCheck> {
    Json(HealthCheck {
        daemon_version: daemon::version(),
    })
}

/// Login a user. If successful a cookie will be return
///
/// E.g.
/// curl -d "password=password" -X POST http://localhost:8000/api/login
#[rocket::post("/login", data = "<form>")]
pub async fn post_login(auth: Auth<'_>, form: Form<Login>) -> Result<Json<User>, HttpApiProblem> {
    let user = auth.login(&form).await?;
    Ok(Json(user))
}

#[rocket::post("/change-password", data = "<form>")]
pub async fn change_password(
    mut user: User,
    auth: Auth<'_>,
    form: Form<ChangePassword>,
) -> Result<(), HttpApiProblem> {
    form.clone().is_secure().map_err(|error| {
        tracing::error!("{error:#}");
        HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Invalid password format")
            .detail(format!("{error:#}"))
    })?;

    user.set_password(&form.password).map_err(|error| {
        HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Could not set password")
            .detail(format!("{error:#}"))
    })?;
    auth.users.update_user(user).await.map_err(|error| {
        HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Could update user password")
            .detail(format!("{error:#}"))
    })?;
    Ok(())
}

#[derive(Serialize, Copy, Clone)]
pub struct Authenticated {
    authenticated: bool,
    first_login: bool,
}

#[rocket::get("/am-I-authenticated")]
pub async fn is_authenticated(auth: Auth<'_>) -> Result<Json<Authenticated>, HttpApiProblem> {
    let authenticated = auth.is_auth().map_err(|error| {
        HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Could not check authentication")
            .detail(format!("{error:#}"))
    })?;
    let first_login = auth
        .get_user()
        .await
        .map_err(|error| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Could not get user from session")
                .detail(format!("{error:#}"))
        })?
        .map(|user| user.first_login)
        .unwrap_or_default();
    Ok(Json(Authenticated {
        authenticated,
        first_login,
    }))
}

#[rocket::get("/logout")]
pub fn logout(auth: Auth<'_>) -> Result<(), HttpApiProblem> {
    auth.logout().map_err(|error| {
        HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Could not logout")
            .detail(format!("{error:#}"))
    })?;
    Ok(())
}
