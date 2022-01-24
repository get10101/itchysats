use anyhow::Context;
use anyhow::Result;
use bdk::bitcoin;
use bdk::bitcoin::secp256k1::schnorrsig;
use bdk::bitcoin::Address;
use bdk::bitcoin::Amount;
use bdk::FeeRate;
use clap::Parser;
use clap::Subcommand;
use daemon::bitmex_price_feed;
use daemon::connection::connect;
use daemon::db;
use daemon::logger;
use daemon::model::cfd::Role;
use daemon::model::Identity;
use daemon::monitor;
use daemon::oracle;
use daemon::projection;
use daemon::seed::RandomSeed;
use daemon::seed::Seed;
use daemon::seed::UmbrelSeed;
use daemon::wallet;
use daemon::TakerActorSystem;
use daemon::Tasks;
use daemon::HEARTBEAT_INTERVAL;
use daemon::N_PAYOUTS;
use daemon::SETTLEMENT_INTERVAL;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use tracing_subscriber::filter::LevelFilter;
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

    /// Password for the web interface.
    ///
    /// If not provided, will be derived from the seed.
    #[clap(long)]
    password: Option<rocket_basicauth::Password>,

    #[clap(subcommand)]
    network: Network,

    #[clap(short, long, parse(try_from_str = parse_umbrel_seed))]
    umbrel_seed: Option<[u8; 32]>,
}

fn parse_x25519_pubkey(s: &str) -> Result<x25519_dalek::PublicKey> {
    let mut bytes = [0u8; 32];
    hex::decode_to_slice(s, &mut bytes)?;
    Ok(x25519_dalek::PublicKey::from(bytes))
}

fn parse_umbrel_seed(s: &str) -> Result<[u8; 32]> {
    let mut bytes = [0u8; 32];
    hex::decode_to_slice(s, &mut bytes)?;
    Ok(bytes)
}

#[derive(Parser)]
enum Network {
    Mainnet {
        /// URL to the electrum backend to use for the wallet.
        #[clap(long, default_value = "ssl://blockstream.info:700")]
        electrum: String,

        #[clap(subcommand)]
        withdraw: Option<Withdraw>,
    },
    Testnet {
        /// URL to the electrum backend to use for the wallet.
        #[clap(long, default_value = "ssl://blockstream.info:993")]
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
    let settlement_interval_hours = SETTLEMENT_INTERVAL.whole_hours();

    tracing::info!(
        "CFDs created with this release will settle after {settlement_interval_hours} hours"
    );

    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("unable to get cwd"));

    let data_dir = opts.network.data_dir(data_dir);

    if !data_dir.exists() {
        tokio::fs::create_dir_all(&data_dir).await?;
    }

    let maker_identity = Identity::new(opts.maker_id);

    let bitcoin_network = opts.network.bitcoin_network();
    let (ext_priv_key, identity_sk, web_password) = match opts.umbrel_seed {
        Some(seed_bytes) => {
            let seed = UmbrelSeed::from(seed_bytes);
            let ext_priv_key = seed.derive_extended_priv_key(bitcoin_network)?;
            let (_, identity_sk) = seed.derive_identity();
            let web_password = opts.password.unwrap_or_else(|| seed.derive_auth_password());
            (ext_priv_key, identity_sk, web_password)
        }
        None => {
            let seed = RandomSeed::initialize(&data_dir.join("taker_seed")).await?;
            let ext_priv_key = seed.derive_extended_priv_key(bitcoin_network)?;
            let (_, identity_sk) = seed.derive_identity();
            let web_password = opts.password.unwrap_or_else(|| seed.derive_auth_password());
            (ext_priv_key, identity_sk, web_password)
        }
    };

    let mut tasks = Tasks::default();

    let (wallet, wallet_feed_receiver) = wallet::Actor::new(opts.network.electrum(), ext_priv_key)?;

    let (wallet, wallet_fut) = wallet.create(None).run();
    tasks.add(wallet_fut);

    if let Some(Withdraw::Withdraw {
        amount,
        address,
        fee,
    }) = opts.network.withdraw()
    {
        wallet
            .send(wallet::Withdraw {
                amount: *amount,
                address: address.clone(),
                fee: fee.map(FeeRate::from_sat_per_vb),
            })
            .await??;

        return Ok(());
    }

    let auth_username = rocket_basicauth::Username("itchysats");
    tracing::info!("Authentication details: username='{auth_username}' password='{web_password}'");

    // TODO: Actually fetch it from Olivia
    let oracle = schnorrsig::PublicKey::from_str(
        "ddd4636845a90185991826be5a494cde9f4a6947b1727217afedc6292fa4caf7",
    )?;

    let figment = rocket::Config::figment()
        .merge(("address", opts.http_address.ip()))
        .merge(("port", opts.http_address.port()));

    let db = db::connect(data_dir.join("taker.sqlite")).await?;

    // Create actors

    let (projection_actor, projection_context) = xtra::Context::new(None);

    let taker = TakerActorSystem::new(
        db.clone(),
        wallet.clone(),
        oracle,
        identity_sk,
        |channel| oracle::Actor::new(db.clone(), channel, SETTLEMENT_INTERVAL),
        {
            |channel| {
                let electrum = opts.network.electrum().to_string();
                monitor::Actor::new(db.clone(), electrum, channel)
            }
        },
        bitmex_price_feed::Actor::new,
        N_PAYOUTS,
        HEARTBEAT_INTERVAL,
        Duration::from_secs(10),
        projection_actor.clone(),
        maker_identity,
    )?;

    let (proj_actor, projection_feeds) = projection::Actor::new(
        db.clone(),
        Role::Taker,
        bitcoin_network,
        &taker.price_feed_actor,
    );
    tasks.add(projection_context.run(proj_actor));

    let possible_addresses = resolve_maker_addresses(&opts.maker).await?;

    tasks.add(connect(
        taker.maker_online_status_feed_receiver.clone(),
        taker.connection_actor.clone(),
        maker_identity,
        possible_addresses,
    ));

    let rocket = rocket::custom(figment)
        .manage(projection_feeds)
        .manage(wallet_feed_receiver)
        .manage(bitcoin_network)
        .manage(taker.maker_online_status_feed_receiver.clone())
        .manage(taker)
        .manage(auth_username)
        .manage(web_password)
        .mount(
            "/api",
            rocket::routes![
                routes_taker::feed,
                routes_taker::post_order_request,
                routes_taker::get_health_check,
                routes_taker::post_cfd_action,
                routes_taker::post_withdraw_request,
            ],
        )
        .register("/api", rocket::catchers![rocket_basicauth::unauthorized])
        .mount(
            "/",
            rocket::routes![routes_taker::dist, routes_taker::index],
        )
        .register("/", rocket::catchers![rocket_basicauth::unauthorized]);

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
