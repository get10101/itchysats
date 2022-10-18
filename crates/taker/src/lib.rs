use crate::bitcoin::util::bip32::ExtendedPrivKey;
use crate::routes::IdentityInfo;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use daemon::bdk::bitcoin;
use daemon::bdk::FeeRate;
use daemon::libp2p_utils::create_connect_tcp_multiaddr;
use daemon::monitor;
use daemon::oracle;
use daemon::projection;
use daemon::seed;
use daemon::seed::AppSeed;
use daemon::seed::RandomSeed;
use daemon::seed::Seed;
use daemon::seed::ThreadSafeSeed;
use daemon::wallet;
use daemon::wallet::TAKER_WALLET_ID;
use daemon::Environment;
use daemon::TakerActorSystem;
use daemon::N_PAYOUTS;
use libp2p_core::PeerId;
use model::olivia;
use model::Identity;
use model::Role;
use model::SETTLEMENT_INTERVAL;
use rocket::async_trait;
use rocket_cookie_auth::users::Users;
use shared_bin::catchers::default_catchers;
use shared_bin::cli::Network;
use shared_bin::cli::Withdraw;
use shared_bin::fairings;
use shared_bin::logger;
use shared_bin::logger::LevelFilter;
use shared_bin::logger::LOCAL_COLLECTOR_ENDPOINT;
use shared_bin::MAINNET_ELECTRUM;
use shared_bin::TESTNET_ELECTRUM;
use std::convert::Infallible;
use std::env;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio_extras::Tasks;
use xtras::supervisor::always_restart;
use xtras::supervisor::Supervisor;

mod routes;

pub const ANNOUNCEMENT_LOOKAHEAD: time::Duration = time::Duration::hours(24);

const MAINNET_MAKER: &str = "mainnet.itchysats.network:10001";
const MAINNET_MAKER_ID: &str = "7e35e34801e766a6a29ecb9e22810ea4e3476c2b37bf75882edf94a68b1d9607";
const MAINNET_MAKER_PEER_ID: &str = "12D3KooWP3BN6bq9jPy8cP7Grj1QyUBfr7U6BeQFgMwfTTu12wuY";

const TESTNET_MAKER: &str = "testnet.itchysats.network:10000";
const TESTNET_MAKER_ID: &str = "69a42aa90da8b065b9532b62bff940a3ba07dbbb11d4482c7db83a7e049a9f1e";
const TESTNET_MAKER_PEER_ID: &str = "12D3KooWEsK2X8Tp24XtyWh7DM65VfwXtNH2cmfs2JsWmkmwKbV1";

#[derive(Clone, Debug)]
pub struct Password(String);

impl From<[u8; 32]> for Password {
    fn from(bytes: [u8; 32]) -> Self {
        Self(hex::encode(bytes))
    }
}

impl FromStr for Password {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

impl std::fmt::Display for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Parser)]
pub struct Opts {
    /// The IP address or hostname of the other party (i.e. the maker).
    ///
    /// If not specified it defaults to the itchysats maker for the mainnet or testnet.
    #[clap(long)]
    maker: Option<String>,

    /// The public key of the maker as a 32 byte hex string.
    ///
    /// If not specified it defaults to the itchysats maker-id for mainnet or testnet.
    #[clap(long, value_parser(parse_x25519_pubkey))]
    maker_id: Option<x25519_dalek::PublicKey>,

    /// Maker's peer id, required for establishing libp2p encrypted connection.
    ///
    /// If not specified it defaults to the itchysats maker-peer-id for mainnet or testnet.
    #[clap(long)]
    maker_peer_id: Option<PeerId>,

    /// The IP address to listen on for the HTTP API.
    #[clap(long, default_value = "127.0.0.1:8000")]
    http_address: SocketAddr,

    /// Where to permanently store data, defaults to the current working directory.
    #[clap(long)]
    data_dir: Option<PathBuf>,

    /// If enabled logs will be in json format
    #[clap(short, long)]
    json: bool,

    /// If enabled, logs in json format will contain a list of all ancestor spans of log events.
    /// This **only** has an effect when `json` is also enabled.
    #[clap(long)]
    pub json_span_list: bool,

    /// If enabled, traces will be exported to the OTEL collector
    #[clap(long)]
    instrumentation: bool,

    /// If enabled, tokio runtime can be locally debugged with tokio_console
    #[clap(long)]
    pub tokio_console: bool,

    /// If enabled, libp2p ping and offer broadcast spans will be included in the traces exported
    /// by the application.
    #[clap(long)]
    pub verbose_spans: bool,

    /// OTEL collector endpoint address
    ///
    /// If not specified it defaults to the local collector endpoint.
    #[clap(long, default_value = LOCAL_COLLECTOR_ENDPOINT )]
    collector_endpoint: String,

    /// If enabled, browser UI is not automatically launched at startup.
    #[clap(long)]
    pub headless: bool,

    /// Service name for OTEL.
    ///
    /// If not specified it defaults to the binary name.
    #[clap(long, default_value = "taker")]
    service_name: String,

    /// Configure the log level, e.g.: one of Error, Warn, Info, Debug, Trace
    #[clap(short, long, default_value = "Debug")]
    log_level: LevelFilter,

    /// Password for the web interface.
    ///
    /// If not provided, will be derived from the seed.
    #[clap(long)]
    password: Option<Password>,

    #[clap(subcommand)]
    network: Option<Network>,

    #[clap(short, long, value_parser(parse_app_seed))]
    app_seed: Option<[u8; 32]>,

    /// If provided will be used for internal wallet instead of a random key or app_seed. The
    /// keys will be derived according to Bip84.
    #[clap(short, long)]
    pub wallet_xprv: Option<ExtendedPrivKey>,

    /// If enabled, the log will be printed to {service_name}.log in the data dir
    #[clap(long)]
    pub log_to_file: bool,
}

impl Opts {
    // use this method to parse the options from the cli.
    pub fn read() -> Opts {
        Opts::parse()
    }

    // use this method to construct the options from parameters.
    pub fn new(network: String, data_dir: String, port: u16) -> Result<Self> {
        let network = PublicNetwork::from_str(&network)?;

        let maker = Self::maker_url(&network);
        let maker_id = Self::maker_id(&network);
        let maker_peer_id = Self::maker_peer_id(&network);

        Ok(Self {
            maker: Some(maker),
            maker_id: Some(maker_id),
            maker_peer_id: Some(maker_peer_id),
            http_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port),
            data_dir: Some(PathBuf::from(data_dir)),
            json: false,
            json_span_list: false,
            instrumentation: false,
            tokio_console: false,
            verbose_spans: false,
            collector_endpoint: LOCAL_COLLECTOR_ENDPOINT.to_string(),
            headless: true,
            service_name: "taker".to_string(),
            log_level: LevelFilter::DEBUG,
            password: None,
            network: Some(network.into()),
            app_seed: None,
            wallet_xprv: None,
            log_to_file: true,
        })
    }

    fn network(&self) -> Network {
        self.network.clone().unwrap_or_default()
    }

    fn maker(&self) -> Result<(String, x25519_dalek::PublicKey, PeerId)> {
        let network = PublicNetwork::try_from(self.network())?;

        let maker_url = self
            .maker
            .clone()
            .unwrap_or_else(|| Self::maker_url(&network));

        let maker_id = self.maker_id.unwrap_or_else(|| Self::maker_id(&network));

        let maker_peer_id = self
            .maker_peer_id
            .unwrap_or_else(|| Self::maker_peer_id(&network));

        Ok((maker_url, maker_id, maker_peer_id))
    }

    fn maker_url(network: &PublicNetwork) -> String {
        match network {
            PublicNetwork::Mainnet { .. } => MAINNET_MAKER.to_string(),
            PublicNetwork::Testnet { .. } => TESTNET_MAKER.to_string(),
        }
    }

    fn maker_id(network: &PublicNetwork) -> x25519_dalek::PublicKey {
        match network {
            PublicNetwork::Mainnet { .. } => parse_x25519_pubkey(MAINNET_MAKER_ID).unwrap(),
            PublicNetwork::Testnet { .. } => parse_x25519_pubkey(TESTNET_MAKER_ID).unwrap(),
        }
    }

    fn maker_peer_id(network: &PublicNetwork) -> PeerId {
        match network {
            PublicNetwork::Mainnet { .. } => MAINNET_MAKER_PEER_ID.parse().unwrap(),
            PublicNetwork::Testnet { .. } => TESTNET_MAKER_PEER_ID.parse().unwrap(),
        }
    }
}

enum PublicNetwork {
    Mainnet,
    Testnet,
}

impl TryFrom<Network> for PublicNetwork {
    type Error = anyhow::Error;

    fn try_from(value: Network) -> Result<Self, Self::Error> {
        Ok(match value {
            Network::Mainnet { .. } => Self::Mainnet,
            Network::Testnet { .. } => Self::Testnet,
            private => bail!("Not a public network: {private:?}"),
        })
    }
}

impl From<PublicNetwork> for Network {
    fn from(public: PublicNetwork) -> Self {
        match public {
            PublicNetwork::Mainnet => Network::Mainnet {
                electrum: MAINNET_ELECTRUM.to_string(),
                withdraw: None,
            },
            PublicNetwork::Testnet => Network::Testnet {
                electrum: TESTNET_ELECTRUM.to_string(),
                withdraw: None,
            },
        }
    }
}

impl FromStr for PublicNetwork {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "mainnet" => Self::Mainnet,
            "testnet" => Self::Testnet,
            private => bail!("Not a public network: {private:?}"),
        })
    }
}

fn parse_x25519_pubkey(s: &str) -> Result<x25519_dalek::PublicKey> {
    let mut bytes = [0u8; 32];
    hex::decode_to_slice(s, &mut bytes)?;
    Ok(x25519_dalek::PublicKey::from(bytes))
}

fn parse_app_seed(s: &str) -> Result<[u8; 32]> {
    let mut bytes = [0u8; 32];
    hex::decode_to_slice(s, &mut bytes)?;
    Ok(bytes)
}

pub async fn run(opts: Opts) -> Result<()> {
    let (maker_url, maker_id, maker_peer_id) = opts.maker()?;

    let network = opts.network();

    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("unable to get cwd"));

    let data_dir = network.data_dir(data_dir);

    if !data_dir.exists() {
        tokio::fs::create_dir_all(&data_dir).await?;
    }

    let _guard = logger::init(
        opts.log_level,
        opts.json,
        opts.json_span_list,
        opts.instrumentation,
        opts.tokio_console,
        opts.verbose_spans,
        &opts.service_name,
        &opts.collector_endpoint,
        opts.log_to_file,
        data_dir.to_str().expect("missing data dir"),
    )
    .context("initialize logger")?;
    tracing::info!("Running version: {}", daemon::version());
    let settlement_interval_hours = SETTLEMENT_INTERVAL.whole_hours();

    tracing::info!(
        "CFDs created with this release will settle after {settlement_interval_hours} hours"
    );

    let maker_identity = Identity::new(maker_id);

    let bitcoin_network = network.bitcoin_network();

    let seed: Arc<ThreadSafeSeed> = match opts.app_seed {
        Some(seed_bytes) => Arc::new(AppSeed::from(seed_bytes)),
        None => Arc::new(RandomSeed::initialize(&data_dir.join(seed::TAKER_WALLET_SEED_FILE)).await?),
    };

    let identities = seed.derive_identities();

    let ext_priv_key = match opts.wallet_xprv {
        Some(wallet_xprv) => {
            if wallet_xprv.network != bitcoin_network {
                let network = wallet_xprv.network;
                bail!("Invalid private key provided. Was '{network}' but should have been '{bitcoin_network}'");
            }
            wallet_xprv
        }
        None => seed.derive_extended_priv_key(bitcoin_network)?,
    };

    let mut tasks = Tasks::default();

    let mut wallet_dir = data_dir.clone();
    wallet_dir.push(TAKER_WALLET_ID);
    let (wallet, wallet_feed_receiver) = wallet::Actor::spawn(
        network.electrum(),
        ext_priv_key,
        wallet_dir,
        wallet_seed.is_managed(),
    )?;

    if let Some(Withdraw::Withdraw {
        amount,
        address,
        fee,
    }) = network.withdraw()
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

    let figment = rocket::Config::figment()
        .merge(("address", opts.http_address.ip()))
        .merge(("port", opts.http_address.port()))
        .merge(("cli_colors", false))
        .merge(("secret_key", RandomSeed::default().seed()));

    let db = sqlite_db::connect(data_dir.join("taker.sqlite"), true).await?;

    // Create actors

    let possible_addresses = resolve_maker_addresses(maker_url.as_str()).await?;

    // Assume that the first resolved ipv4 address is good enough for libp2p.
    let maker_libp2p_address = possible_addresses
        .iter()
        .find(|x| x.is_ipv4())
        .context("Could not resolve maker URL")?;
    let maker_multiaddr = create_connect_tcp_multiaddr(maker_libp2p_address, maker_peer_id)?;

    let hex_pk = hex::encode(identities.identity_pk.to_bytes());
    let peer_id = identities.libp2p.public().to_peer_id().to_string();

    tracing::info!("Connection details: taker_id='{hex_pk}', peer_id='{peer_id}'");

    let identity_info = IdentityInfo {
        taker_id: hex_pk,
        taker_peer_id: peer_id,
    };

    let environment = match env::var("ITCHYSATS_ENV") {
        Ok(environment) => Environment::new(environment.as_str()),
        Err(_) => Environment::new("binary"),
    };

    let (supervisor, price_feed_actor) =
        Supervisor::<_, xtra_bitmex_price_feed::Error>::with_policy(
            {
                let network = network.bitmex_network();
                move || xtra_bitmex_price_feed::Actor::new(network)
            },
            always_restart(),
        );

    tasks.add(supervisor.run_log_summary());

    let (feed_senders, feed_receivers) = projection::feeds();
    let feed_senders = Arc::new(feed_senders);

    let (supervisor, projection_actor) = Supervisor::new({
        let db = db.clone();
        let price_feed = price_feed_actor.clone();
        move || {
            projection::Actor::new(
                db.clone(),
                bitcoin_network,
                price_feed.clone().into(),
                Role::Taker,
                feed_senders.clone(),
            )
        }
    });
    tasks.add(supervisor.run_log_summary());

    let taker = TakerActorSystem::new(
        db.clone(),
        wallet.clone(),
        *olivia::PUBLIC_KEY,
        identities,
        |executor| oracle::Actor::new(db.clone(), executor),
        |executor| {
            let electrum = network.electrum().to_string();
            monitor::Actor::new(db.clone(), electrum, executor)
        },
        price_feed_actor,
        N_PAYOUTS,
        Duration::from_secs(10),
        projection_actor.clone(),
        maker_identity,
        maker_multiaddr,
        environment,
    )?;

    if let Some(password) = opts.password {
        db.clone()
            .update_password(rocket_cookie_auth::user::create_password(
                password.to_string().as_str(),
            )?)
            .await?;
    }

    let rocket_auth_db_connection = RocketAuthDbConnection::new(db.clone());
    let users = Users::new(Box::new(rocket_auth_db_connection));

    let mut rocket = rocket::custom(figment)
        .manage(feed_receivers)
        .manage(wallet_feed_receiver)
        .manage(identity_info)
        .manage(bitcoin_network)
        .manage(taker.maker_online_status_feed_receiver.clone())
        .manage(taker.identify_info_feed_receiver.clone())
        .manage(taker)
        .mount(
            "/api",
            rocket::routes![
                routes::feed,
                routes::post_order_request,
                routes::post_cfd_action,
                routes::post_withdraw_request,
                routes::put_sync_wallet,
                shared_bin::routes::get_health_check,
                shared_bin::routes::get_metrics,
                shared_bin::routes::get_version,
                shared_bin::routes::change_password,
                shared_bin::routes::post_login,
                shared_bin::routes::logout,
                shared_bin::routes::is_authenticated,
            ],
        )
        .register("/api", default_catchers())
        .manage(users)
        .manage(data_dir)
        .manage(network)
        .mount("/", rocket::routes![routes::dist, routes::index])
        .register("/", default_catchers())
        .attach(fairings::log_launch())
        .attach(fairings::log_requests())
        .attach(fairings::ui_browser_launch(!opts.headless));

    if wallet_seed.is_managed() {
        rocket = rocket.mount(
            "/api",
            rocket::routes![routes::get_export_seed, routes::put_import_seed],
        );
    }

    let mission_success = rocket.launch().await?;
    tracing::trace!(?mission_success, "Rocket has landed");

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

struct RocketAuthDbConnection {
    inner: sqlite_db::Connection,
}

impl RocketAuthDbConnection {
    fn new(db: sqlite_db::Connection) -> Self {
        Self { inner: db }
    }
}

#[async_trait]
impl rocket_cookie_auth::Database for RocketAuthDbConnection {
    async fn load_user(&self) -> Result<Option<rocket_cookie_auth::user::User>> {
        let users = self.inner.clone().load_user().await?;
        Ok(users.map(|user| rocket_cookie_auth::user::User {
            id: user.id,
            password: user.password,
            auth_key: rocket_cookie_auth::NO_AUTH_KEY_SET.to_string(),
            first_login: user.first_login,
        }))
    }

    async fn update_password(&self, password: String) -> Result<()> {
        self.inner.clone().update_password(password).await?;
        Ok(())
    }
}
