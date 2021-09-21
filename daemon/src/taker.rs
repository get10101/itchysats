use crate::model::WalletInfo;
use crate::taker_cfd_actor::Command;
use crate::wallet::Wallet;
use anyhow::Result;
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

mod db;
mod keypair;
mod model;
mod routes;
mod routes_taker;
mod seed;
mod send_wire_message_actor;
mod setup_contract_actor;
mod taker_cfd_actor;
mod taker_inc_message_actor;
mod to_sse_event;
mod wallet;
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
}

#[rocket::main]
async fn main() -> Result<()> {
    let opts = Opts::parse();

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
    let wallet_info = wallet.sync().unwrap();

    let oracle = schnorrsig::KeyPair::new(SECP256K1, &mut rand::thread_rng()); // TODO: Fetch oracle public key from oracle.

    let (cfd_feed_sender, cfd_feed_receiver) = watch::channel::<Vec<Cfd>>(vec![]);
    let (order_feed_sender, order_feed_receiver) = watch::channel::<Option<Order>>(None);
    let (wallet_feed_sender, wallet_feed_receiver) = watch::channel::<WalletInfo>(wallet_info);

    let (read, write) = loop {
        let socket = tokio::net::TcpSocket::new_v4()?;
        if let Ok(connection) = socket.connect(opts.taker).await {
            break connection.into_split();
        } else {
            println!(
                "Could not connect to the maker, retrying in {}s ...",
                CONNECTION_RETRY_INTERVAL.as_secs()
            );
            sleep(CONNECTION_RETRY_INTERVAL);
        }
    };

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

                let (out_maker_messages_actor, out_maker_actor_inbox) =
                    send_wire_message_actor::new(write);
                let (cfd_actor, cfd_actor_inbox) = taker_cfd_actor::new(
                    db,
                    wallet,
                    schnorrsig::PublicKey::from_keypair(SECP256K1, &oracle),
                    cfd_feed_sender,
                    order_feed_sender,
                    out_maker_actor_inbox,
                    wallet_feed_sender,
                );
                let inc_maker_messages_actor =
                    taker_inc_message_actor::new(read, cfd_actor_inbox.clone());

                // consecutive wallet syncs handled by task that triggers sync
                let wallet_sync_interval = Duration::from_secs(10);
                tokio::spawn({
                    let cfd_actor_inbox = cfd_actor_inbox.clone();
                    async move {
                        loop {
                            cfd_actor_inbox.send(Command::SyncWallet).unwrap();
                            tokio::time::sleep(wallet_sync_interval).await;
                        }
                    }
                });

                tokio::spawn(cfd_actor);
                tokio::spawn(inc_maker_messages_actor);
                tokio::spawn(out_maker_messages_actor);

                Ok(rocket.manage(cfd_actor_inbox))
            },
        ))
        .mount(
            "/api",
            rocket::routes![
                routes_taker::feed,
                routes_taker::post_cfd,
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
