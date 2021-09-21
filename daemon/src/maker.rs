use crate::maker_cfd_actor::Command;
use crate::seed::Seed;
use crate::wallet::Wallet;
use anyhow::Result;
use bdk::bitcoin::secp256k1::{schnorrsig, SECP256K1};
use bdk::bitcoin::Network;
use clap::Clap;
use model::cfd::{Cfd, Order};
use model::WalletInfo;
use rocket::fairing::AdHoc;
use rocket_db_pools::Database;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

mod db;
mod keypair;
mod maker_cfd_actor;
mod maker_inc_connections_actor;
mod model;
mod routes_maker;
mod seed;
mod send_wire_message_actor;
mod setup_contract_actor;
mod to_sse_event;
mod wallet;
mod wire;

#[derive(Database)]
#[database("maker")]
pub struct Db(sqlx::SqlitePool);

#[derive(Clap)]
struct Opts {
    /// The port to listen on for p2p connections.
    #[clap(long, default_value = "9999")]
    p2p_port: u16,

    /// The port to listen on for the HTTP API.
    #[clap(long, default_value = "8001")]
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

    let seed = Seed::initialize(&data_dir.join("maker_seed"), opts.generate_seed).await?;

    let ext_priv_key = seed.derive_extended_priv_key(Network::Testnet)?;

    let wallet = Wallet::new(
        &opts.electrum,
        &data_dir.join("maker_wallet_db"),
        ext_priv_key,
    )
    .await?;
    let wallet_info = wallet.sync().unwrap();

    let oracle = schnorrsig::KeyPair::new(SECP256K1, &mut rand::thread_rng()); // TODO: Fetch oracle public key from oracle.

    let (cfd_feed_sender, cfd_feed_receiver) = watch::channel::<Vec<Cfd>>(vec![]);
    let (order_feed_sender, order_feed_receiver) = watch::channel::<Option<Order>>(None);
    let (wallet_feed_sender, wallet_feed_receiver) = watch::channel::<WalletInfo>(wallet_info);

    let figment = rocket::Config::figment()
        .merge(("databases.maker.url", data_dir.join("maker.sqlite")))
        .merge(("port", opts.http_port));

    let listener = tokio::net::TcpListener::bind(&format!("0.0.0.0:{}", opts.p2p_port)).await?;
    let local_addr = listener.local_addr().unwrap();

    println!("Listening on {}", local_addr);

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

                let (connections_actor_inbox_sender, connections_actor_inbox_recv) =
                    mpsc::unbounded_channel();

                let (cfd_maker_actor, cfd_maker_actor_inbox) = maker_cfd_actor::new(
                    db,
                    wallet,
                    schnorrsig::PublicKey::from_keypair(SECP256K1, &oracle),
                    connections_actor_inbox_sender,
                    cfd_feed_sender,
                    order_feed_sender,
                    wallet_feed_sender,
                );
                let connections_actor = maker_inc_connections_actor::new(
                    listener,
                    cfd_maker_actor_inbox.clone(),
                    connections_actor_inbox_recv,
                );

                // consecutive wallet syncs handled by task that triggers sync
                let wallet_sync_interval = Duration::from_secs(10);
                tokio::spawn({
                    let cfd_actor_inbox = cfd_maker_actor_inbox.clone();
                    async move {
                        loop {
                            cfd_actor_inbox.send(Command::SyncWallet).unwrap();
                            tokio::time::sleep(wallet_sync_interval).await;
                        }
                    }
                });

                tokio::spawn(cfd_maker_actor);
                tokio::spawn(connections_actor);

                Ok(rocket.manage(cfd_maker_actor_inbox))
            },
        ))
        .mount(
            "/api",
            rocket::routes![
                routes_maker::maker_feed,
                routes_maker::post_sell_order,
                // routes_maker::post_confirm_order,
                routes_maker::get_health_check
            ],
        )
        .mount(
            "/",
            rocket::routes![routes_maker::dist, routes_maker::index],
        )
        .launch()
        .await?;

    Ok(())
}
