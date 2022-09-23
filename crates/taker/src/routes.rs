#![allow(clippy::let_unit_value)]
// see: https://github.com/SergioBenitez/Rocket/issues/2211
use daemon::bdk;
use daemon::bdk::bitcoin::Amount;
use daemon::bdk::bitcoin::Network;
use daemon::bdk::blockchain::ElectrumBlockchain;
use daemon::bdk::sled;
use daemon::identify;
use daemon::online_status::ConnectionStatus;
use daemon::oracle;
use daemon::projection;
use daemon::projection::CfdAction;
use daemon::projection::FeedReceivers;
use daemon::seed::ThreadSafeSeed;
use daemon::wallet;
use daemon::TakerActorSystem;
use http_api_problem::HttpApiProblem;
use http_api_problem::StatusCode;
use model::Contracts;
use model::Leverage;
use model::OrderId;
use model::Price;
use model::Timestamp;
use model::WalletInfo;
use rocket::form::Form;
use rocket::http::ContentType;
use rocket::http::Status;
use rocket::response::stream::Event;
use rocket::response::stream::EventStream;
use rocket::response::Responder;
use rocket::serde::json::Json;
use rocket::serde::uuid::Uuid;
use rocket::State;
use rocket_cookie_auth::auth::Auth;
use rocket_cookie_auth::basic::BasicAuthGuard;
use rocket_cookie_auth::forms::ChangePassword;
use rocket_cookie_auth::forms::Login;
use rocket_cookie_auth::user::User;
use rocket_download_response::mime;
use rocket_download_response::DownloadResponsePro;
use rust_embed::RustEmbed;
use rust_embed_rocket::EmbeddedFileExt;
use serde::Deserialize;
use serde::Serialize;
use shared_bin::ToSseEvent;
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::select;
use tokio::sync::watch;
use tracing::instrument;

type Taker = TakerActorSystem<
    oracle::Actor,
    wallet::Actor<ElectrumBlockchain, sled::Tree>,
    xtra_bitmex_price_feed::Actor,
>;

const HEARTBEAT_INTERVAL_SECS: u64 = 5;

#[derive(Debug, Clone, Serialize)]
pub struct IdentityInfo {
    /// legacy networking identity
    pub(crate) taker_id: String,
    /// libp2p peer id
    pub(crate) taker_peer_id: String,
}

#[rocket::get("/feed")]
#[instrument(name = "GET /feed", skip_all)]
pub async fn feed(
    rx: &State<FeedReceivers>,
    rx_wallet: &State<watch::Receiver<Option<WalletInfo>>>,
    rx_maker_status: &State<watch::Receiver<ConnectionStatus>>,
    rx_maker_identity: &State<watch::Receiver<Option<identify::PeerInfo>>>,
    identity_info: &State<IdentityInfo>,
    _user: User,
) -> EventStream![] {
    let rx = rx.inner();
    let mut rx_cfds = rx.cfds.clone();
    let mut rx_offers = rx.offers.clone();

    let mut rx_wallet = rx_wallet.inner().clone();
    let mut rx_maker_status = rx_maker_status.inner().clone();
    let mut rx_maker_identity = rx_maker_identity.inner().clone();
    let identity = identity_info.inner().clone();
    let mut heartbeat =
        tokio::time::interval(std::time::Duration::from_secs(HEARTBEAT_INTERVAL_SECS));

    EventStream! {

        let wallet_info = rx_wallet.borrow().clone();
        yield wallet_info.to_sse_event();

        let maker_status = rx_maker_status.borrow().clone();
        yield maker_status.to_sse_event();

        let maker_identity = rx_maker_identity.borrow().clone();
        yield maker_identity.to_sse_event();

        yield Event::json(&identity).event("identity");

        let offers = rx_offers.borrow().clone();
        yield Event::json(&offers.btcusd_long).event("btcusd_long_offer");
        yield Event::json(&offers.btcusd_short).event("btcusd_short_offer");
        yield Event::json(&offers.ethusd_long).event("ethusd_long_offer");
        yield Event::json(&offers.ethusd_short).event("ethusd_short_offer");

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
                Ok(()) = rx_maker_status.changed() => {
                    let maker_status = rx_maker_status.borrow().clone();
                    yield maker_status.to_sse_event();
                },
                Ok(()) = rx_maker_identity.changed() => {
                    let maker_identity = rx_maker_identity.borrow().clone();
                    yield maker_identity.to_sse_event();
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
                _ = heartbeat.tick() => {
                    yield Event::json(&Heartbeat::new()).event("heartbeat")
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Heartbeat {
    timestamp: Timestamp,
    interval: u64,
}

impl Heartbeat {
    pub fn new() -> Self {
        Self {
            timestamp: Timestamp::now(),
            interval: HEARTBEAT_INTERVAL_SECS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CfdOrderRequest {
    pub order_id: OrderId,
    pub quantity: Contracts,
    pub leverage: Leverage,
}

#[rocket::post("/cfd/order", data = "<cfd_order_request>")]
#[instrument(name = "POST /cfd/order", skip(taker, _user), err)]
pub async fn post_order_request(
    cfd_order_request: Json<CfdOrderRequest>,
    taker: &State<Taker>,
    _user: User,
) -> Result<(), HttpApiProblem> {
    taker
        .place_order(
            cfd_order_request.order_id,
            cfd_order_request.quantity,
            cfd_order_request.leverage,
        )
        .await
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Order request failed")
                .detail(format!("{e:#}"))
        })?;

    Ok(())
}

#[rocket::post("/cfd/<order_id>/<action>")]
#[instrument(name = "POST /cfd/<order_id>/<action>", skip(taker, _user), err)]
pub async fn post_cfd_action(
    order_id: Uuid,
    action: String,
    taker: &State<Taker>,
    _user: User,
) -> Result<(), HttpApiProblem> {
    let order_id = OrderId::from(order_id);
    let action = action.parse().map_err(|_| {
        HttpApiProblem::new(StatusCode::BAD_REQUEST).detail(format!("Invalid action: {}", action))
    })?;

    let result = match action {
        CfdAction::AcceptOrder
        | CfdAction::RejectOrder
        | CfdAction::AcceptSettlement
        | CfdAction::RejectSettlement => {
            return Err(HttpApiProblem::new(StatusCode::BAD_REQUEST)
                .detail(format!("taker cannot invoke action {action}")));
        }
        CfdAction::Commit => taker.commit(order_id).await,
        CfdAction::Settle => taker.propose_settlement(order_id).await,
    };

    result.map_err(|e| {
        HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title(action.to_string() + " failed")
            .detail(format!("{e:#}"))
    })?;

    Ok(())
}

#[rocket::get("/alive")]
#[instrument(name = "GET /alive")]
pub fn get_health_check() {}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct MarginRequest {
    pub price: Price,
    pub quantity: Contracts,
    pub leverage: Leverage,

    #[serde(with = "bdk::bitcoin::util::amount::serde::as_btc")]
    pub opening_fee: Amount,
}

/// Represents the collateral that has to be put up
#[derive(Debug, Clone, Copy, Serialize)]
pub struct MarginResponse {
    #[serde(with = "bdk::bitcoin::util::amount::serde::as_btc")]
    pub margin: Amount,

    /// Margin + fees
    #[serde(with = "bdk::bitcoin::util::amount::serde::as_btc")]
    pub complete_initial_costs: Amount,
}

#[derive(RustEmbed)]
#[folder = "../../taker-frontend/dist/taker"]
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

#[derive(Debug, Clone, Deserialize)]
pub struct WithdrawRequest {
    address: bdk::bitcoin::Address,
    #[serde(with = "bdk::bitcoin::util::amount::serde::as_btc")]
    amount: Amount,
    fee: f32,
}

#[rocket::post("/withdraw", data = "<withdraw_request>")]
#[instrument(name = "POST /withdraw", skip(taker, _user), err)]
pub async fn post_withdraw_request(
    withdraw_request: Json<WithdrawRequest>,
    taker: &State<Taker>,
    network: &State<Network>,
    _user: User,
) -> Result<String, HttpApiProblem> {
    let amount =
        (withdraw_request.amount != bdk::bitcoin::Amount::ZERO).then(|| withdraw_request.amount);

    let txid = taker
        .withdraw(
            amount,
            withdraw_request.address.clone(),
            bdk::FeeRate::from_sat_per_vb(withdraw_request.fee),
        )
        .await
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Could not proceed with withdraw request")
                .detail(format!("{e:#}"))
        })?;

    Ok(projection::to_mempool_url(txid, *network.inner()))
}

#[rocket::get("/metrics")]
#[instrument(name = "GET /metrics", skip_all, err)]
pub async fn get_metrics<'r>(_basic_auth: BasicAuthGuard) -> Result<String, HttpApiProblem> {
    let metrics = prometheus::TextEncoder::new()
        .encode_to_string(&prometheus::gather())
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to encode metrics")
                .detail(e.to_string())
        })?;

    Ok(metrics)
}

#[rocket::put("/sync")]
#[instrument(name = "PUT /sync", skip_all, err)]
pub async fn put_sync_wallet(taker: &State<Taker>, _user: User) -> Result<(), HttpApiProblem> {
    taker.sync_wallet().await.map_err(|e| {
        HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Could not sync wallet")
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

#[rocket::get("/backup")]
#[instrument(name = "GET /backup", skip_all)]
pub async fn get_seed_backup(
    seed: &State<Arc<ThreadSafeSeed>>,
    _user: User,
) -> Result<DownloadResponsePro, HttpApiProblem> {
    let resp = DownloadResponsePro::from_vec(
        seed.inner().seed(),
        Some("taker_seed"),
        Some(mime::APPLICATION_OCTET_STREAM),
    );
    Ok(resp)
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

#[derive(Serialize)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_test::Token;

    #[test]
    fn heartbeat_serialization() {
        let heartbeat = Heartbeat {
            timestamp: Timestamp::new(0),
            interval: 1,
        };

        serde_test::assert_ser_tokens(
            &heartbeat,
            &[
                Token::Struct {
                    name: "Heartbeat",
                    len: 2,
                },
                Token::Str("timestamp"),
                Token::NewtypeStruct { name: "Timestamp" },
                Token::I64(0),
                Token::Str("interval"),
                Token::U64(1),
                Token::StructEnd,
            ],
        );
    }
}
