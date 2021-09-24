use crate::model::WalletInfo;
use crate::wallet::Wallet;
use anyhow::{Context, Result};
use bdk::bitcoin::secp256k1::{schnorrsig, SECP256K1};
use bdk::bitcoin::Network;
use clap::Clap;
use model::cfd::{Cfd, Order};
use rocket::fairing::AdHoc;
use rocket_db_pools::Database;
use seed::Seed;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;
use tokio::sync::watch;
use tracing_subscriber::filter::LevelFilter;
use wire::TakerToMaker;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor;

mod actors;
mod bitmex_price_feed;
mod db;
mod keypair;
mod logger;
mod model;
mod routes;
mod routes_taker;
mod seed;
mod send_to_socket;
mod setup_contract_actor;
mod taker_cfd;
mod taker_inc_message_actor;
mod to_sse_event;
mod wallet;
mod wallet_sync;
mod wire;

const CONNECTION_RETRY_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Database)]
#[database("taker")]
pub struct Db(sqlx::SqlitePool);

#[derive(Clap)]
struct Opts {
    /// The IP address of the taker to connect to.
    #[clap(long, default_value = "127.0.0.1:9999")]
    taker: SocketAddr,

    /// The port to listen on for the HTTP API.
    #[clap(long, default_value = "8000")]
    http_port: u16,

    /// URL to the electrum backend to use for the wallet.
    #[clap(long, default_value = "ssl://electrum.blockstream.info:60002")]
    electrum: String,

    /// Where to permanently store data, defaults to the current working directory.
    #[clap(long)]
    data_dir: Option<PathBuf>,

    /// Generate a seed file within the data directory.
    #[clap(long)]
    generate_seed: bool,

    /// If enabled logs will be in json format
    #[clap(short, long)]
    json: bool,
}

#[rocket::main]
async fn main() -> Result<()> {
    let opts = Opts::parse();

    logger::init(LevelFilter::DEBUG, opts.json).context("initialize logger")?;

    let data_dir = opts
        .data_dir
        .unwrap_or_else(|| std::env::current_dir().expect("unable to get cwd"));

    if !data_dir.exists() {
        tokio::fs::create_dir_all(&data_dir).await?;
    }

    let seed = Seed::initialize(&data_dir.join("taker_seed"), opts.generate_seed).await?;

    let ext_priv_key = seed.derive_extended_priv_key(Network::Testnet)?;

    let wallet = Wallet::new(
        &opts.electrum,
        &data_dir.join("taker_wallet_db"),
        ext_priv_key,
    )
    .await?;
    let wallet_info = wallet.sync().await.unwrap();

    let oracle = schnorrsig::KeyPair::new(SECP256K1, &mut rand::thread_rng()); // TODO: Fetch oracle public key from oracle.

    let (cfd_feed_sender, cfd_feed_receiver) = watch::channel::<Vec<Cfd>>(vec![]);
    let (order_feed_sender, order_feed_receiver) = watch::channel::<Option<Order>>(None);
    let (wallet_feed_sender, wallet_feed_receiver) = watch::channel::<WalletInfo>(wallet_info);

    let (read, write) = loop {
        let socket = tokio::net::TcpSocket::new_v4()?;
        if let Ok(connection) = socket.connect(opts.taker).await {
            break connection.into_split();
        } else {
            tracing::warn!(
                "Could not connect to the maker, retrying in {}s ...",
                CONNECTION_RETRY_INTERVAL.as_secs()
            );
            sleep(CONNECTION_RETRY_INTERVAL);
        }
    };

    let (task, mut quote_updates) = bitmex_price_feed::new().await?;
    tokio::spawn(task);

    // dummy usage of quote receiver
    tokio::spawn(async move {
        loop {
            let bitmex_price_feed::Quote { bid, ask, .. } = *quote_updates.borrow();
            tracing::info!(%bid, %ask, "BitMex quote updated");

            if quote_updates.changed().await.is_err() {
                return;
            }
        }
    });

    let figment = rocket::Config::figment()
        .merge(("databases.taker.url", data_dir.join("taker.sqlite")))
        .merge(("port", opts.http_port));

    rocket::custom(figment)
        .manage(cfd_feed_receiver)
        .manage(order_feed_receiver)
        .manage(wallet_feed_receiver)
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

                let send_to_maker = send_to_socket::Actor::new(write)
                    .create(None)
                    .spawn_global();

                let cfd_actor_inbox = taker_cfd::Actor::new(
                    db,
                    wallet.clone(),
                    schnorrsig::PublicKey::from_keypair(SECP256K1, &oracle),
                    cfd_feed_sender,
                    order_feed_sender,
                    send_to_maker,
                )
                .await
                .unwrap()
                .create(None)
                .spawn_global();

                let inc_maker_messages_actor =
                    taker_inc_message_actor::new(read, cfd_actor_inbox.clone());

                tokio::spawn(wallet_sync::new(wallet, wallet_feed_sender));
                tokio::spawn(inc_maker_messages_actor);

                Ok(rocket.manage(cfd_actor_inbox))
            },
        ))
        .mount(
            "/api",
            rocket::routes![
                routes_taker::feed,
                routes_taker::post_order_request,
                routes_taker::get_health_check,
                routes_taker::margin_calc,
            ],
        )
        .mount(
            "/",
            rocket::routes![routes_taker::dist, routes_taker::index],
        )
        .launch()
        .await?;

    Ok(())
}

impl xtra::Message for TakerToMaker {
    type Result = ();
}
