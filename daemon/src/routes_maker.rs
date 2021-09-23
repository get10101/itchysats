use crate::auth::Authenticated;
use crate::maker_cfd_actor::{self, MakerCfdActor};
use crate::model::cfd::{Cfd, Order, Origin};
use crate::model::{Usd, WalletInfo};
use crate::routes::EmbeddedFileExt;
use crate::to_sse_event::ToSseEvent;
use anyhow::Result;
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
use xtra::Address;

#[rocket::get("/feed")]
pub async fn maker_feed(
    rx_cfds: &State<watch::Receiver<Vec<Cfd>>>,
    rx_order: &State<watch::Receiver<Option<Order>>>,
    rx_wallet: &State<watch::Receiver<WalletInfo>>,
    _auth: Authenticated,
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
    cfd_actor_address: &State<Address<MakerCfdActor>>,
    _auth: Authenticated,
) -> Result<status::Accepted<()>, status::BadRequest<String>> {
    let order = Order::from_default_with_price(order.price, Origin::Ours)
        .map_err(|e| status::BadRequest(Some(e.to_string())))?
        .with_min_quantity(order.min_quantity)
        .with_max_quantity(order.max_quantity);

    cfd_actor_address
        .do_send_async(maker_cfd_actor::NewOrder(order))
        .await
        .expect("actor to always be available");

    Ok(status::Accepted(None))
}

/// Test route solely for the purposes of exercising authentication.
/// It validates whether the posted order request was correct, but does not do
/// anything else with it.
#[cfg(test)]
#[rocket::post("/order/test", data = "<order>")]
pub async fn post_test_order(
    order: Json<CfdNewOrderRequest>,
    _auth: Authenticated,
) -> Result<status::Accepted<()>, status::BadRequest<String>> {
    let _order = Order::from_default_with_price(order.price, Origin::Ours)
        .map_err(|e| status::BadRequest(Some(e.to_string())))?
        .with_min_quantity(order.min_quantity)
        .with_max_quantity(order.max_quantity);

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

#[derive(RustEmbed)]
#[folder = "../frontend/dist/maker"]
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
    use crate::auth::Password;
    use bdk::bitcoin::{Address, Amount, Network, PublicKey};
    use rocket::http::{Header, Status};
    use rocket::local::blocking::Client;
    use rocket::{Build, Rocket};
    use std::time::SystemTime;

    #[test]
    fn routes_are_password_protected() {
        let client = Client::tracked(rocket()).unwrap();

        let feed_response = client.get("/feed").dispatch();
        let new_sell_order_response = client
            .post("/order/test")
            .body(r#"{"price":"40000", "min_quantity":"100", "max_quantity":"10000"}"#)
            .dispatch();
        let index_response = client.get("/").header(ContentType::HTML).dispatch();

        assert_eq!(feed_response.status(), Status::Unauthorized);
        assert_eq!(new_sell_order_response.status(), Status::Unauthorized);
        assert_eq!(index_response.status(), Status::Unauthorized);
    }

    #[test]
    fn correct_password_grants_access() {
        let client = Client::tracked(rocket()).unwrap();

        let feed_response = client.get("/feed").header(auth_header()).dispatch();
        let new_sell_order_response = client
            .post("/order/test")
            .body(r#"{"price":"40000", "min_quantity":"100", "max_quantity":"10000"}"#)
            .header(auth_header())
            .dispatch();
        let index_response = client
            .get("/")
            .header(ContentType::HTML)
            .header(auth_header())
            .dispatch();

        assert_eq!(feed_response.status(), Status::Ok);
        assert_eq!(new_sell_order_response.status(), Status::Accepted);
        assert!(
            index_response.status() == Status::NotFound || index_response.status() == Status::Ok
        );
    }

    /// Constructs a Rocket instance for testing.
    fn rocket() -> Rocket<Build> {
        let (_, state1) = watch::channel::<Vec<Cfd>>(vec![]);
        let (_, state2) = watch::channel::<Option<Order>>(None);
        let (_, state3) = watch::channel::<WalletInfo>(WalletInfo {
            balance: Amount::ZERO,
            address: Address::p2wpkh(
                &PublicKey::new(
                    "0286cd889349ebc06b3165505b9c083df0a4147f554614ff207c10f16ff509578c"
                        .parse()
                        .unwrap(),
                ),
                Network::Regtest,
            )
            .unwrap(),
            last_updated_at: SystemTime::now(),
        });

        rocket::build()
            .manage(state1)
            .manage(state2)
            .manage(state3)
            .manage(Password::from(*b"Now I'm feelin' so fly like a G6"))
            .mount("/", rocket::routes![maker_feed, post_test_order, index])
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
