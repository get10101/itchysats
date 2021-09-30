use crate::db::load_all_cfds;
use crate::model::WalletInfo;
use crate::wallet::Wallet;
use anyhow::{Context, Result};
use bdk::bitcoin::secp256k1::{schnorrsig, SECP256K1};
use bdk::bitcoin::Network;
use clap::Clap;
use futures::StreamExt;
use model::cfd::{Cfd, Order};
use rocket::fairing::AdHoc;
use rocket_db_pools::Database;
use seed::Seed;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::codec::FramedRead;
use tracing_subscriber::filter::LevelFilter;
use wire::TakerToMaker;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor;

mod actors;
mod bitmex_price_feed;
mod cleanup;
mod db;
mod keypair;
mod logger;
mod model;
mod monitor;
mod oracle;
mod routes;
mod routes_taker;
mod seed;
mod send_to_socket;
mod setup_contract;
mod taker_cfd;
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
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("unable to get cwd"));

    if !data_dir.exists() {
        tokio::fs::create_dir_all(&data_dir).await?;
    }

    let seed = Seed::initialize(&data_dir.join("taker_seed"), opts.generate_seed).await?;

    let ext_priv_key = seed.derive_extended_priv_key(Network::Testnet)?;

    let wallet = Wallet::new(
        &opts.electrum,
        &data_dir.join("taker_wallet.sqlite"),
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

    let (task, quote_updates) = bitmex_price_feed::new().await?;
    tokio::spawn(task);

    let figment = rocket::Config::figment()
        .merge(("databases.taker.url", data_dir.join("taker.sqlite")))
        .merge(("port", opts.http_port));

    rocket::custom(figment)
        .manage(cfd_feed_receiver)
        .manage(order_feed_receiver)
        .manage(wallet_feed_receiver)
        .manage(quote_updates)
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

                cleanup::transition_non_continue_cfds_to_setup_failed(db.clone())
                    .await
                    .unwrap();

                let send_to_maker = send_to_socket::Actor::new(write)
                    .create(None)
                    .spawn_global();

                let (monitor_actor_address, monitor_actor_context) = xtra::Context::new(None);

                let (oracle_actor_address, mut oracle_actor_context) = xtra::Context::new(None);

                let mut conn = db.acquire().await.unwrap();
                let cfds = load_all_cfds(&mut conn).await.unwrap();
                let cfd_actor_inbox = taker_cfd::Actor::new(
                    db.clone(),
                    wallet.clone(),
                    schnorrsig::PublicKey::from_keypair(SECP256K1, &oracle),
                    cfd_feed_sender,
                    order_feed_sender,
                    send_to_maker,
                    monitor_actor_address.clone(),
                    cfds.clone(),
                    oracle_actor_address,
                )
                .await
                .unwrap()
                .create(None)
                .spawn_global();

                let read = FramedRead::new(read, wire::JsonCodec::new())
                    .map(move |item| taker_cfd::MakerStreamMessage { item });

                tokio::spawn(cfd_actor_inbox.clone().attach_stream(read));
                tokio::spawn(
                    monitor_actor_context.run(
                        monitor::Actor::new(&opts.electrum, cfd_actor_inbox.clone(), cfds).await,
                    ),
                );
                tokio::spawn(wallet_sync::new(wallet, wallet_feed_sender));

                tokio::spawn(
                    oracle_actor_context
                        .notify_interval(Duration::from_secs(60), || oracle::Sync)
                        .unwrap(),
                );
                tokio::spawn(oracle_actor_context.run(oracle::Actor::new(
                    cfd_actor_inbox.clone(),
                    monitor_actor_address,
                )));

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
