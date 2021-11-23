use anyhow::{Context, Result};
use bdk::bitcoin::secp256k1::schnorrsig;
use bdk::bitcoin::{Address, Amount};
use bdk::{bitcoin, FeeRate};
use clap::{Parser, Subcommand};
use daemon::bitmex_price_feed::Quote;
use daemon::connection::connect;
use daemon::db::load_all_cfds;
use daemon::model::cfd::{Order, UpdateCfdProposals};
use daemon::model::WalletInfo;
use daemon::seed::Seed;
use daemon::tokio_ext::FutureExt;
use daemon::{
    bitmex_price_feed, db, housekeeping, logger, monitor, oracle, projection, taker_cfd, wallet,
    wallet_sync, TakerActorSystem, Tasks, HEARTBEAT_INTERVAL, N_PAYOUTS,
};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use tokio::sync::watch;
use tracing_subscriber::filter::LevelFilter;
use watch::channel;
use xtra::prelude::MessageChannel;
use xtra::Actor;

mod routes_taker;

pub const ANNOUNCEMENT_LOOKAHEAD: time::Duration = time::Duration::hours(24);

#[derive(Parser)]
struct Opts {
    /// The IP address or hostname of the other party (i.e. the maker).
    #[clap(long)]
    maker: String,

    /// The public key of the maker as a 32 byte hex string.
    #[clap(long, parse(try_from_str = parse_x25519_pubkey))]
    maker_id: x25519_dalek::PublicKey,

    /// The IP address to listen on for the HTTP API.
    #[clap(long, default_value = "127.0.0.1:8000")]
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

    #[clap(subcommand)]
    network: Network,
}

fn parse_x25519_pubkey(s: &str) -> Result<x25519_dalek::PublicKey> {
    let mut bytes = [0u8; 32];
    hex::decode_to_slice(s, &mut bytes)?;
    Ok(x25519_dalek::PublicKey::from(bytes))
}

#[derive(Parser)]
enum Network {
    Mainnet {
        /// URL to the electrum backend to use for the wallet.
        #[clap(long, default_value = "ssl://electrum.blockstream.info:50002")]
        electrum: String,

        #[clap(subcommand)]
        withdraw: Option<Withdraw>,
    },
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
        address: Address,
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

    let seed = Seed::initialize(&data_dir.join("taker_seed")).await?;

    let bitcoin_network = opts.network.bitcoin_network();
    let ext_priv_key = seed.derive_extended_priv_key(bitcoin_network)?;
    let (_, identity_sk) = seed.derive_identity();

    let (wallet, wallet_fut) = wallet::Actor::new(
        opts.network.electrum(),
        &data_dir.join("taker_wallet.sqlite"),
        ext_priv_key,
    )
    .await?
    .create(None)
    .run();
    let _wallet_handle = wallet_fut.spawn_with_handle();

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

    // TODO: Actually fetch it from Olivia
    let oracle = schnorrsig::PublicKey::from_str(
        "ddd4636845a90185991826be5a494cde9f4a6947b1727217afedc6292fa4caf7",
    )?;

    let (wallet_feed_sender, wallet_feed_receiver) = watch::channel::<WalletInfo>(wallet_info);

    let mut tasks = Tasks::default();

    let figment = rocket::Config::figment()
        .merge(("address", opts.http_address.ip()))
        .merge(("port", opts.http_address.port()));

    let db = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .create_if_missing(true)
            .filename(data_dir.join("taker.sqlite")),
    )
    .await?;

    db::run_migrations(&db)
        .await
        .context("Db migrations failed")?;

    // Create actors
    let mut conn = db.acquire().await?;

    housekeeping::transition_non_continue_cfds_to_setup_failed(&mut conn).await?;
    housekeeping::rebroadcast_transactions(&mut conn, &wallet).await?;

    let (projection_actor, projection_context) = xtra::Context::new(None);

    let TakerActorSystem {
        cfd_actor_addr,
        connection_actor_addr,
        maker_online_status_feed_receiver,
        tasks: _tasks,
    } = TakerActorSystem::new(
        db.clone(),
        wallet.clone(),
        oracle,
        identity_sk,
        |cfds, channel| oracle::Actor::new(cfds, channel, ANNOUNCEMENT_LOOKAHEAD),
        {
            |channel, cfds| {
                let electrum = opts.network.electrum().to_string();
                monitor::Actor::new(electrum, channel, cfds)
            }
        },
        N_PAYOUTS,
        HEARTBEAT_INTERVAL * 2,
        projection_actor.clone(),
    )
    .await?;
    let (task, init_quote) = bitmex_price_feed::new(projection_actor).await?;
    tasks.add(task);

    let cfds = load_all_cfds(&mut conn).await?;
    let (cfd_feed_sender, cfd_feed_receiver) = channel(cfds.clone());
    let (order_feed_sender, order_feed_receiver) = channel::<Option<Order>>(None);
    let (update_cfd_feed_sender, update_cfd_feed_receiver) =
        channel::<UpdateCfdProposals>(HashMap::new());
    let (quote_sender, quote_receiver) = channel::<Quote>(init_quote);

    tasks.add(projection_context.run(projection::Actor::new(
        cfd_feed_sender,
        order_feed_sender,
        quote_sender,
        update_cfd_feed_sender,
    )));

    let possible_addresses = resolve_maker_addresses(&opts.maker).await?;

    tasks.add(connect(
        maker_online_status_feed_receiver.clone(),
        connection_actor_addr,
        opts.maker_id,
        possible_addresses,
    ));

    tasks.add(wallet_sync::new(wallet, wallet_feed_sender));
    let take_offer_channel = MessageChannel::<taker_cfd::TakeOffer>::clone_channel(&cfd_actor_addr);
    let cfd_action_channel = MessageChannel::<taker_cfd::CfdAction>::clone_channel(&cfd_actor_addr);

    let rocket = rocket::custom(figment)
        .manage(order_feed_receiver)
        .manage(update_cfd_feed_receiver)
        .manage(take_offer_channel)
        .manage(cfd_action_channel)
        .manage(cfd_feed_receiver)
        .manage(wallet_feed_receiver)
        .manage(quote_receiver)
        .manage(bitcoin_network)
        .manage(maker_online_status_feed_receiver)
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
        );

    let rocket = rocket.ignite().await?;
    rocket.launch().await?;

    db.close().await;

    Ok(())
}

async fn resolve_maker_addresses(maker_addr: &str) -> Result<Vec<SocketAddr>> {
    let possible_addresses = tokio::net::lookup_host(maker_addr)
        .await?
        .collect::<Vec<_>>();

    tracing::debug!(
        "Resolved {} to [{}]",
        maker_addr,
        itertools::join(possible_addresses.iter(), ",")
    );
    Ok(possible_addresses)
}
