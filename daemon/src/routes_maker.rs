use anyhow::Result;
use bdk::bitcoin::Network;
use daemon::auth::Authenticated;
use daemon::model::cfd::{Cfd, Order, OrderId, Role, UpdateCfdProposals};
use daemon::model::{Price, Usd, WalletInfo};
use daemon::routes::EmbeddedFileExt;
use daemon::to_sse_event::{CfdAction, CfdsWithAuxData, ToSseEvent};
use daemon::{bitmex_price_feed, maker_cfd};
use http_api_problem::{HttpApiProblem, StatusCode};
use rocket::http::{ContentType, Header, Status};
use rocket::response::stream::EventStream;
use rocket::response::{status, Responder};
use rocket::serde::json::Json;
use rocket::State;
use rust_embed::RustEmbed;
use serde::Deserialize;
use std::borrow::Cow;
use std::path::PathBuf;
use tokio::select;
use tokio::sync::watch;
use xtra::prelude::*;

#[rocket::get("/feed")]
pub async fn maker_feed(
    rx_cfds: &State<watch::Receiver<Vec<Cfd>>>,
    rx_order: &State<watch::Receiver<Option<Order>>>,
    rx_wallet: &State<watch::Receiver<WalletInfo>>,
    rx_quote: &State<watch::Receiver<bitmex_price_feed::Quote>>,
    rx_settlements: &State<watch::Receiver<UpdateCfdProposals>>,
    network: &State<Network>,
    _auth: Authenticated,
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
            Role::Maker, network
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
                        Role::Maker,
                        network
                    ).to_sse_event();
                }
                Ok(()) = rx_settlements.changed() => {
                    yield CfdsWithAuxData::new(
                        &rx_cfds,
                        &rx_quote,
                        &rx_settlements,
                        Role::Maker,
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
                        Role::Maker,
                        network
                    ).to_sse_event();
                }
            }
        }
    }
}

/// The maker POSTs this to create a new CfdOrder
// TODO: Use Rocket form?
#[derive(Debug, Clone, Deserialize)]
pub struct CfdNewOrderRequest {
    pub price: Price,
    // TODO: [post-MVP] Representation of the contract size; at the moment the contract size is
    // always 1 USD
    pub min_quantity: Usd,
    pub max_quantity: Usd,
}

#[rocket::post("/order/sell", data = "<order>")]
pub async fn post_sell_order(
    order: Json<CfdNewOrderRequest>,
    new_order_channel: &State<Box<dyn MessageChannel<maker_cfd::NewOrder>>>,
    _auth: Authenticated,
) -> Result<status::Accepted<()>, HttpApiProblem> {
    new_order_channel
        .send(maker_cfd::NewOrder {
            price: order.price,
            min_quantity: order.min_quantity,
            max_quantity: order.max_quantity,
        })
        .await
        .unwrap_or_else(|e| anyhow::bail!(e))
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Posting offer failed")
                .detail(e.to_string())
        })?;

    Ok(status::Accepted(None))
}

/// A "catcher" for all 401 responses, triggers the browser's basic auth implementation.
#[rocket::catch(401)]
pub fn unauthorized() -> PromptAuthentication {
    PromptAuthentication {
        inner: (),
        www_authenticate: Header::new("WWW-Authenticate", r#"Basic charset="UTF-8"#),
    }
}

/// A rocket responder that prompts the user to sign in to access the API.
#[derive(rocket::Responder)]
#[response(status = 401)]
pub struct PromptAuthentication {
    inner: (),
    www_authenticate: Header<'static>,
}

#[rocket::post("/cfd/<id>/<action>")]
pub async fn post_cfd_action(
    id: OrderId,
    action: CfdAction,
    cfd_action_channel: &State<Box<dyn MessageChannel<maker_cfd::CfdAction>>>,
    _auth: Authenticated,
) -> Result<status::Accepted<()>, HttpApiProblem> {
    use maker_cfd::CfdAction::*;
    let result = match action {
        CfdAction::AcceptOrder => cfd_action_channel.send(AcceptOrder { order_id: id }),
        CfdAction::RejectOrder => cfd_action_channel.send(RejectOrder { order_id: id }),
        CfdAction::AcceptSettlement => cfd_action_channel.send(AcceptSettlement { order_id: id }),
        CfdAction::RejectSettlement => cfd_action_channel.send(RejectSettlement { order_id: id }),
        CfdAction::AcceptRollOver => cfd_action_channel.send(AcceptRollOver { order_id: id }),
        CfdAction::RejectRollOver => cfd_action_channel.send(RejectRollOver { order_id: id }),
        CfdAction::Commit => cfd_action_channel.send(Commit { order_id: id }),
        CfdAction::Settle => {
            let msg = "Collaborative settlement can only be triggered by taker";
            tracing::error!(msg);
            return Err(HttpApiProblem::new(StatusCode::BAD_REQUEST).detail(msg));
        }
    };

    result
        .await
        .unwrap_or_else(|e| anyhow::bail!(e))
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title(action.to_string() + " failed")
                .detail(e.to_string())
        })?;

    Ok(status::Accepted(None))
}

#[rocket::get("/alive")]
pub fn get_health_check() {}

#[derive(RustEmbed)]
#[folder = "../maker-frontend/dist/maker"]
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

#[cfg(test)]
mod tests {
    use super::*;
    use daemon::auth::Password;
    use rocket::http::{Header, Status};
    use rocket::local::blocking::Client;
    use rocket::{Build, Rocket};

    #[test]
    fn routes_are_password_protected() {
        let client = Client::tracked(rocket()).unwrap();

        let response = client.get("/protected").dispatch();

        assert_eq!(response.status(), Status::Unauthorized);
        assert_eq!(
            response.headers().get_one("WWW-Authenticate"),
            Some(r#"Basic charset="UTF-8"#)
        );
    }

    #[test]
    fn correct_password_grants_access() {
        let client = Client::tracked(rocket()).unwrap();

        let response = client.get("/protected").header(auth_header()).dispatch();

        assert_eq!(response.status(), Status::Ok);
    }

    #[rocket::get("/protected")]
    async fn protected(_auth: Authenticated) {}

    /// Constructs a Rocket instance for testing.
    fn rocket() -> Rocket<Build> {
        rocket::build()
            .manage(Password::from(*b"Now I'm feelin' so fly like a G6"))
            .mount("/", rocket::routes![protected])
            .register("/", rocket::catchers![unauthorized])
    }

    /// Creates an "Authorization" header that matches the password above,
    /// in particular it has been created through:
    /// ```
    /// base64(maker:hex("Now I'm feelin' so fly like a G6"))
    /// ```
    fn auth_header() -> Header<'static> {
        Header::new(
            "Authorization",
            "Basic bWFrZXI6NGU2Zjc3MjA0OTI3NmQyMDY2NjU2NTZjNjk2ZTI3MjA3MzZmMjA2NjZjNzkyMDZjNjk2YjY1MjA2MTIwNDczNg==",
        )
    }
}
