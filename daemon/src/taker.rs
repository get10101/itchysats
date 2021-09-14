use anyhow::Result;
use bdk::bitcoin::secp256k1::{schnorrsig, SECP256K1};
use bdk::bitcoin::{self, Amount};
use bdk::blockchain::{ElectrumBlockchain, NoopProgress};
use model::cfd::{Cfd, CfdOffer};
use rocket::fairing::AdHoc;
use rocket::figment::util::map;
use rocket::figment::value::{Map, Value};
use rocket_db_pools::Database;
use tokio::sync::watch;

mod db;
mod keypair;
mod model;
mod routes_taker;
mod send_wire_message_actor;
mod taker_cfd_actor;
mod taker_inc_message_actor;
mod to_sse_event;
mod wire;

#[derive(Database)]
#[database("taker")]
pub struct Db(sqlx::SqlitePool);

#[rocket::main]
async fn main() -> Result<()> {
    let client =
        bdk::electrum_client::Client::new("ssl://electrum.blockstream.info:60002").unwrap();

    // TODO: Replace with sqlite once https://github.com/bitcoindevkit/bdk/pull/376 is merged.
    let db = bdk::sled::open("/tmp/taker.db")?;
    let wallet_db = db.open_tree("wallet")?;

    let wallet = bdk::Wallet::new(
        "wpkh(tprv8ZgxMBicQKsPfL3BRRo2gK3rMQwsy49vhEHCsaRJSM3gNrwnDwpdzLVQzbsDo738VHyrMK3FJAaxsBkpu8gk77SUQ197RNyF46brV2EVKRZ/*)#29cd5ajg",
        None,
        bitcoin::Network::Testnet,
        wallet_db,
        ElectrumBlockchain::from(client),
    )
    .unwrap();
    wallet.sync(NoopProgress, None).unwrap(); // TODO: Use LogProgress once we have logging.

    let oracle = schnorrsig::KeyPair::new(SECP256K1, &mut rand::thread_rng()); // TODO: Fetch oracle public key from oracle.

    let (cfd_feed_sender, cfd_feed_receiver) = watch::channel::<Vec<Cfd>>(vec![]);
    let (offer_feed_sender, offer_feed_receiver) = watch::channel::<Option<CfdOffer>>(None);
    let (_balance_feed_sender, balance_feed_receiver) = watch::channel::<Amount>(Amount::ZERO);

    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    let connection = socket
        .connect("127.0.0.1:9999".parse().unwrap())
        .await
        .expect("Maker should be online first");

    let (read, write) = connection.into_split();

    let db: Map<_, Value> = map! {
        "url" => "./taker.sqlite".into(),
    };

    let figment = rocket::Config::figment().merge(("databases", map!["taker" => db]));

    rocket::custom(figment)
        .manage(cfd_feed_receiver)
        .manage(offer_feed_receiver)
        .manage(balance_feed_receiver)
        .attach(Db::init())
        .attach(AdHoc::try_on_ignite(
            "SQL migrations",
            |rocket| async move {
                match Db::fetch(&rocket) {
                    Some(db) => match db::run_migrations(&**db).await {
                        Ok(_) => Ok(rocket),
                        Err(_) => Err(rocket),
                    },
                    None => Err(rocket),
                }
            },
        ))
        .attach(AdHoc::try_on_ignite(
            "Create actors",
            move |rocket| async move {
                let db = match Db::fetch(&rocket) {
                    Some(db) => (**db).clone(),
                    None => return Err(rocket),
                };

                let (out_maker_messages_actor, out_maker_actor_inbox) =
                    send_wire_message_actor::new(write);
                let (cfd_actor, cfd_actor_inbox) = taker_cfd_actor::new(
                    db,
                    wallet,
                    schnorrsig::PublicKey::from_keypair(SECP256K1, &oracle),
                    cfd_feed_sender,
                    offer_feed_sender,
                    out_maker_actor_inbox,
                );
                let inc_maker_messages_actor =
                    taker_inc_message_actor::new(read, cfd_actor_inbox.clone());

                tokio::spawn(cfd_actor);
                tokio::spawn(inc_maker_messages_actor);
                tokio::spawn(out_maker_messages_actor);

                Ok(rocket.manage(cfd_actor_inbox))
            },
        ))
        .mount(
            "/",
            rocket::routes![
                routes_taker::feed,
                routes_taker::post_cfd,
                routes_taker::get_health_check,
                routes_taker::margin_calc,
            ],
        )
        .launch()
        .await?;

    Ok(())
}
