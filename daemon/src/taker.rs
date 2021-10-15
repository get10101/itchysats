use anyhow::{Context, Result};
use bdk::bitcoin;
use bdk::bitcoin::secp256k1::schnorrsig;
use clap::Clap;
use daemon::db::{self, load_all_cfds};
use daemon::model::cfd::{Order, UpdateCfdProposals};
use daemon::model::WalletInfo;
use daemon::seed::Seed;
use daemon::wallet::Wallet;
use daemon::{
    bitmex_price_feed, fan_out, housekeeping, logger, monitor, oracle, send_to_socket, taker_cfd,
    wallet_sync, wire,
};
use futures::StreamExt;
use rocket::fairing::AdHoc;
use rocket_db_pools::Database;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::thread::sleep;
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::codec::FramedRead;
use tracing_subscriber::filter::LevelFilter;
use xtra::prelude::MessageChannel;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor;

mod routes_taker;

const CONNECTION_RETRY_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Database)]
#[database("taker")]
pub struct Db(sqlx::SqlitePool);

#[derive(Clap)]
struct Opts {
    /// The IP address of the other party (i.e. the maker).
    #[clap(long, default_value = "127.0.0.1:9999")]
    maker: SocketAddr,

    /// The IP address to listen on for the HTTP API.
    #[clap(long, default_value = "127.0.0.1:8000")]
    http_address: SocketAddr,

    /// Where to permanently store data, defaults to the current working directory.
    #[clap(long)]
    data_dir: Option<PathBuf>,

    /// Generate a seed file within the data directory.
    #[clap(long)]
    generate_seed: bool,

    /// If enabled logs will be in json format
    #[clap(short, long)]
    json: bool,

    #[clap(subcommand)]
    network: Network,
}

#[derive(Clap)]
enum Network {
    Mainnet {
        /// URL to the electrum backend to use for the wallet.
        #[clap(long, default_value = "ssl://electrum.blockstream.info:50002")]
        electrum: String,
    },
    Testnet {
        /// URL to the electrum backend to use for the wallet.
        #[clap(long, default_value = "ssl://electrum.blockstream.info:60002")]
        electrum: String,
    },
    /// Run on signet
    Signet {
        /// URL to the electrum backend to use for the wallet.
        #[clap(long)]
        electrum: String,
    },
}

impl Network {
    fn electrum(&self) -> &str {
        match self {
            Network::Mainnet { electrum } => electrum,
            Network::Testnet { electrum } => electrum,
            Network::Signet { electrum } => electrum,
        }
    }

    fn bitcoin_network(&self) -> bitcoin::Network {
        match self {
            Network::Mainnet { .. } => bitcoin::Network::Bitcoin,
            Network::Testnet { .. } => bitcoin::Network::Testnet,
            Network::Signet { .. } => bitcoin::Network::Signet,
        }
    }

    fn data_dir(&self, base: PathBuf) -> PathBuf {
        match self {
            Network::Mainnet { .. } => base.join("mainnet"),
            Network::Testnet { .. } => base.join("testnet"),
            Network::Signet { .. } => base.join("signet"),
        }
    }
}

#[rocket::main]
async fn main() -> Result<()> {
    let opts = Opts::parse();

    logger::init(LevelFilter::DEBUG, opts.json).context("initialize logger")?;

    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("unable to get cwd"));

    let data_dir = opts.network.data_dir(data_dir);

    if !data_dir.exists() {
        tokio::fs::create_dir_all(&data_dir).await?;
    }

    let seed = Seed::initialize(&data_dir.join("taker_seed"), opts.generate_seed).await?;

    let bitcoin_network = opts.network.bitcoin_network();
    let ext_priv_key = seed.derive_extended_priv_key(bitcoin_network)?;

    let wallet = Wallet::new(
        opts.network.electrum(),
        &data_dir.join("taker_wallet.sqlite"),
        ext_priv_key,
    )
    .await?;
    let wallet_info = wallet.sync().await.unwrap();

    // TODO: Actually fetch it from Olivia
    let oracle = schnorrsig::PublicKey::from_str(
        "ddd4636845a90185991826be5a494cde9f4a6947b1727217afedc6292fa4caf7",
    )?;

    let (order_feed_sender, order_feed_receiver) = watch::channel::<Option<Order>>(None);
    let (wallet_feed_sender, wallet_feed_receiver) = watch::channel::<WalletInfo>(wallet_info);
    let (update_cfd_feed_sender, update_feed_receiver) =
        watch::channel::<UpdateCfdProposals>(HashMap::new());

    let (read, write) = loop {
        let socket = tokio::net::TcpSocket::new_v4()?;
        if let Ok(connection) = socket.connect(opts.maker).await {
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
        .merge(("address", opts.http_address.ip()))
        .merge(("port", opts.http_address.port()));

    rocket::custom(figment)
        .manage(order_feed_receiver)
        .manage(wallet_feed_receiver)
        .manage(update_feed_receiver)
        .manage(quote_updates)
        .manage(bitcoin_network)
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
                let mut conn = db.acquire().await.unwrap();

                housekeeping::transition_non_continue_cfds_to_setup_failed(&mut conn)
                    .await
                    .unwrap();
                housekeeping::rebroadcast_transactions(&mut conn, &wallet)
                    .await
                    .unwrap();
                let cfds = load_all_cfds(&mut conn).await.unwrap();

                let (cfd_feed_sender, cfd_feed_receiver) = watch::channel(cfds.clone());
                let send_to_maker = send_to_socket::Actor::new(write)
                    .create(None)
                    .spawn_global();

                let (monitor_actor_address, mut monitor_actor_context) = xtra::Context::new(None);
                let (oracle_actor_address, mut oracle_actor_context) = xtra::Context::new(None);

                let mut conn = db.acquire().await.unwrap();
                let cfds = load_all_cfds(&mut conn).await.unwrap();
                let cfd_actor_inbox = taker_cfd::Actor::new(
                    db.clone(),
                    wallet.clone(),
                    oracle,
                    cfd_feed_sender,
                    order_feed_sender,
                    update_cfd_feed_sender,
                    send_to_maker,
                    monitor_actor_address.clone(),
                    oracle_actor_address.clone(),
                )
                .create(None)
                .spawn_global();

                let read = FramedRead::new(read, wire::JsonCodec::default())
                    .map(move |item| taker_cfd::MakerStreamMessage { item });

                tokio::spawn(cfd_actor_inbox.clone().attach_stream(read));
                tokio::spawn(
                    monitor_actor_context
                        .notify_interval(Duration::from_secs(20), || monitor::Sync)
                        .unwrap(),
                );
                tokio::spawn(
                    monitor_actor_context.run(
                        monitor::Actor::new(
                            opts.network.electrum().to_string(),
                            Box::new(cfd_actor_inbox.clone()),
                            cfds.clone(),
                        )
                        .await
                        .unwrap(),
                    ),
                );
                tokio::spawn(wallet_sync::new(wallet, wallet_feed_sender));
                tokio::spawn(
                    oracle_actor_context
                        .notify_interval(Duration::from_secs(5), || oracle::Sync)
                        .unwrap(),
                );
                let actor = fan_out::Actor::new(&[&cfd_actor_inbox, &monitor_actor_address])
                    .create(None)
                    .spawn_global();

                tokio::spawn(oracle_actor_context.run(oracle::Actor::new(cfds, Box::new(actor))));

                oracle_actor_address
                    .do_send_async(oracle::Sync)
                    .await
                    .unwrap();

                let take_offer_channel =
                    MessageChannel::<taker_cfd::TakeOffer>::clone_channel(&cfd_actor_inbox);
                let cfd_action_channel =
                    MessageChannel::<taker_cfd::CfdAction>::clone_channel(&cfd_actor_inbox);
                Ok(rocket
                    .manage(take_offer_channel)
                    .manage(cfd_action_channel)
                    .manage(cfd_feed_receiver))
            },
        ))
        .mount(
            "/api",
            rocket::routes![
                routes_taker::feed,
                routes_taker::post_order_request,
                routes_taker::get_health_check,
                routes_taker::margin_calc,
                routes_taker::post_cfd_action,
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
