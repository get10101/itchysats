use anyhow::{Context, Result};
use bdk::bitcoin::secp256k1::schnorrsig;
use bdk::{bitcoin, FeeRate};
use clap::{Parser, Subcommand};
use daemon::auth::{self, MAKER_USERNAME};
use daemon::db::{self};

use daemon::model::WalletInfo;

use daemon::seed::Seed;
use daemon::{
    bitmex_price_feed, housekeeping, logger, maker_cfd, maker_inc_connections, monitor, oracle,
    wallet, wallet_sync, MakerActorSystem,
};

use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

use bdk::bitcoin::Amount;
use std::task::Poll;
use tokio::sync::watch;
use tracing_subscriber::filter::LevelFilter;
use xtra::prelude::*;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor;

mod routes_maker;

#[derive(Parser)]
struct Opts {
    /// The port to listen on for p2p connections.
    #[clap(long, default_value = "9999")]
    p2p_port: u16,

    /// The IP address to listen on for the HTTP API.
    #[clap(long, default_value = "127.0.0.1:8001")]
    http_address: SocketAddr,

    /// Where to permanently store data, defaults to the current working directory.
    #[clap(long)]
    data_dir: Option<PathBuf>,

    /// If enabled logs will be in json format
    #[clap(short, long)]
    json: bool,

    /// Configure the log level, e.g.: one of Error, Warn, Info, Debug, Trace
    #[clap(short, long, default_value = "Debug")]
    log_level: LevelFilter,

    /// The time interval until potential settlement of each CFD in hours
    #[clap(long, default_value = "24")]
    settlement_time_interval_hours: u8,

    #[clap(subcommand)]
    network: Network,
}

#[derive(Parser)]
enum Network {
    /// Run on mainnet.
    Mainnet {
        /// URL to the electrum backend to use for the wallet.
        #[clap(long, default_value = "ssl://electrum.blockstream.info:50002")]
        electrum: String,

        #[clap(subcommand)]
        withdraw: Option<Withdraw>,
    },
    /// Run on testnet.
    Testnet {
        /// URL to the electrum backend to use for the wallet.
        #[clap(long, default_value = "ssl://electrum.blockstream.info:60002")]
        electrum: String,

        #[clap(subcommand)]
        withdraw: Option<Withdraw>,
    },
    /// Run on signet
    Signet {
        /// URL to the electrum backend to use for the wallet.
        #[clap(long)]
        electrum: String,

        #[clap(subcommand)]
        withdraw: Option<Withdraw>,
    },
}

#[derive(Subcommand)]
enum Withdraw {
    Withdraw {
        /// Optionally specify the amount of Bitcoin to be withdrawn. If not specified the wallet
        /// will be drained. Amount is to be specified with denomination, e.g. "0.1 BTC"
        #[clap(long)]
        amount: Option<Amount>,
        /// Optionally specify the fee-rate for the transaction. The fee-rate is specified as sats
        /// per vbyte, e.g. 5.0
        #[clap(long)]
        fee: Option<f32>,
        /// The address to receive the Bitcoin.
        #[clap(long)]
        address: bdk::bitcoin::Address,
    },
}

impl Network {
    fn electrum(&self) -> &str {
        match self {
            Network::Mainnet { electrum, .. } => electrum,
            Network::Testnet { electrum, .. } => electrum,
            Network::Signet { electrum, .. } => electrum,
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

    fn withdraw(&self) -> &Option<Withdraw> {
        match self {
            Network::Mainnet { withdraw, .. } => withdraw,
            Network::Testnet { withdraw, .. } => withdraw,
            Network::Signet { withdraw, .. } => withdraw,
        }
    }
}

#[rocket::main]
async fn main() -> Result<()> {
    let opts = Opts::parse();

    logger::init(opts.log_level, opts.json).context("initialize logger")?;
    tracing::info!("Running version: {}", env!("VERGEN_GIT_SEMVER_LIGHTWEIGHT"));

    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("unable to get cwd"));

    let data_dir = opts.network.data_dir(data_dir);

    if !data_dir.exists() {
        tokio::fs::create_dir_all(&data_dir).await?;
    }

    let seed = Seed::initialize(&data_dir.join("maker_seed")).await?;

    let bitcoin_network = opts.network.bitcoin_network();
    let ext_priv_key = seed.derive_extended_priv_key(bitcoin_network)?;

    let wallet = wallet::Actor::new(
        opts.network.electrum(),
        &data_dir.join("maker_wallet.sqlite"),
        ext_priv_key,
    )
    .await?
    .create(None)
    .spawn_global();

    // do this before withdraw to ensure the wallet is synced
    let wallet_info = wallet.send(wallet::Sync).await??;

    if let Some(Withdraw::Withdraw {
        amount,
        address,
        fee,
    }) = opts.network.withdraw()
    {
        let txid = wallet
            .send(wallet::Withdraw {
                amount: *amount,
                address: address.clone(),
                fee: fee.map(FeeRate::from_sat_per_vb),
            })
            .await??;

        tracing::info!(%txid, "Withdraw successful");

        return Ok(());
    }

    let auth_password = seed.derive_auth_password::<auth::Password>();

    let noise_static_sk = seed.derive_noise_static_secret();
    let noise_static_pk = x25519_dalek::PublicKey::from(&noise_static_sk);

    tracing::info!(
        "Authentication details: username='{}' password='{}', noise_public_key='{}'",
        MAKER_USERNAME,
        auth_password,
        hex::encode(noise_static_pk.to_bytes())
    );

    // TODO: Actually fetch it from Olivia
    let oracle = schnorrsig::PublicKey::from_str(
        "ddd4636845a90185991826be5a494cde9f4a6947b1727217afedc6292fa4caf7",
    )?;

    let (wallet_feed_sender, wallet_feed_receiver) = watch::channel::<WalletInfo>(wallet_info);

    let figment = rocket::Config::figment()
        .merge(("address", opts.http_address.ip()))
        .merge(("port", opts.http_address.port()));

    let p2p_socket = format!("0.0.0.0:{}", opts.p2p_port)
        .parse::<SocketAddr>()
        .unwrap();
    let listener = tokio::net::TcpListener::bind(p2p_socket)
        .await
        .with_context(|| format!("Failed to listen on {}", p2p_socket))?;
    let local_addr = listener.local_addr().unwrap();

    tracing::info!("Listening on {}", local_addr);

    let (task, quote_updates) = bitmex_price_feed::new().await?;
    tokio::spawn(task);

    let db = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .create_if_missing(true)
            .filename(data_dir.join("maker.sqlite")),
    )
    .await?;

    db::run_migrations(&db)
        .await
        .context("Db migrations failed")?;

    // Create actors
    let mut conn = db.acquire().await?;

    housekeeping::transition_non_continue_cfds_to_setup_failed(&mut conn).await?;
    housekeeping::rebroadcast_transactions(&mut conn, &wallet).await?;

    let settlement_time_interval_hours =
        time::Duration::hours(opts.settlement_time_interval_hours as i64);
    let MakerActorSystem {
        cfd_actor_addr,
        cfd_feed_receiver,
        order_feed_receiver,
        update_cfd_feed_receiver,
        inc_conn_addr: incoming_connection_addr,
    } = MakerActorSystem::new(
        db.clone(),
        wallet.clone(),
        oracle,
        |cfds, channel| oracle::Actor::new(cfds, channel, settlement_time_interval_hours),
        {
            |channel, cfds| {
                let electrum = opts.network.electrum().to_string();
                monitor::Actor::new(electrum, channel, cfds)
            }
        },
        |channel0, channel1| maker_inc_connections::Actor::new(channel0, channel1, noise_static_sk),
        time::Duration::hours(opts.settlement_time_interval_hours as i64),
    )
    .await?;

    let listener_stream = futures::stream::poll_fn(move |ctx| {
        let message = match futures::ready!(listener.poll_accept(ctx)) {
            Ok((stream, address)) => {
                maker_inc_connections::ListenerMessage::NewConnection { stream, address }
            }
            Err(e) => maker_inc_connections::ListenerMessage::Error { source: e },
        };

        Poll::Ready(Some(message))
    });

    tokio::spawn(incoming_connection_addr.attach_stream(listener_stream));

    tokio::spawn(wallet_sync::new(wallet, wallet_feed_sender));

    let cfd_action_channel = MessageChannel::<maker_cfd::CfdAction>::clone_channel(&cfd_actor_addr);
    let new_order_channel = MessageChannel::<maker_cfd::NewOrder>::clone_channel(&cfd_actor_addr);

    rocket::custom(figment)
        .manage(order_feed_receiver)
        .manage(update_cfd_feed_receiver)
        .manage(cfd_action_channel)
        .manage(new_order_channel)
        .manage(cfd_feed_receiver)
        .manage(wallet_feed_receiver)
        .manage(auth_password)
        .manage(quote_updates)
        .manage(bitcoin_network)
        .mount(
            "/api",
            rocket::routes![
                routes_maker::maker_feed,
                routes_maker::post_sell_order,
                routes_maker::post_cfd_action,
                routes_maker::get_health_check
            ],
        )
        .register("/api", rocket::catchers![routes_maker::unauthorized])
        .mount(
            "/",
            rocket::routes![routes_maker::dist, routes_maker::index],
        )
        .register("/", rocket::catchers![routes_maker::unauthorized])
        .launch()
        .await?;

    db.close().await;

    Ok(())
}
