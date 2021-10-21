use anyhow::{Context, Result};
use bdk::bitcoin;
use bdk::bitcoin::secp256k1::schnorrsig;
use clap::Clap;
use daemon::auth::{self, MAKER_USERNAME};
use daemon::db::{self, load_all_cfds};
use daemon::maker_cfd::{FromTaker, NewTakerOnline};
use daemon::model::cfd::{Cfd, Order, UpdateCfdProposals};
use daemon::model::WalletInfo;
use daemon::oracle::Attestation;
use daemon::seed::Seed;
use daemon::wallet::Wallet;
use daemon::{
    bitmex_price_feed, fan_out, housekeeping, logger, maker_cfd, maker_inc_connections, monitor,
    oracle, wallet_sync,
};
use futures::Future;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::task::Poll;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing_subscriber::filter::LevelFilter;
use xtra::prelude::*;
use xtra::spawn::TokioGlobalSpawnExt;

mod routes_maker;

#[derive(Clap)]
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

    /// Generate a seed file within the data directory.
    #[clap(long)]
    generate_seed: bool,

    /// If enabled logs will be in json format
    #[clap(short, long)]
    json: bool,

    /// The term length of each CFD in hours.
    #[clap(long, default_value = "24")]
    term: u8,

    #[clap(subcommand)]
    network: Network,
}

#[derive(Clap)]
enum Network {
    /// Run on mainnet.
    Mainnet {
        /// URL to the electrum backend to use for the wallet.
        #[clap(long, default_value = "ssl://electrum.blockstream.info:50002")]
        electrum: String,
    },
    /// Run on testnet.
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
    tracing::info!("Running version: {}", env!("VERGEN_GIT_SEMVER_LIGHTWEIGHT"));

    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("unable to get cwd"));

    let data_dir = opts.network.data_dir(data_dir);

    if !data_dir.exists() {
        tokio::fs::create_dir_all(&data_dir).await?;
    }

    let seed = Seed::initialize(&data_dir.join("maker_seed"), opts.generate_seed).await?;

    let bitcoin_network = opts.network.bitcoin_network();
    let ext_priv_key = seed.derive_extended_priv_key(bitcoin_network)?;

    let wallet = Wallet::new(
        opts.network.electrum(),
        &data_dir.join("maker_wallet.sqlite"),
        ext_priv_key,
    )
    .await?;
    let wallet_info = wallet.sync().await?;

    let auth_password = seed.derive_auth_password::<auth::Password>();

    tracing::info!(
        "Authentication details: username='{}' password='{}'",
        MAKER_USERNAME,
        auth_password
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

    let ActorSystem {
        cfd_actor_addr,
        cfd_feed_receiver,
        order_feed_receiver,
        update_cfd_feed_receiver,
    } = ActorSystem::new(
        db.clone(),
        wallet.clone(),
        oracle,
        |cfds, channel| oracle::Actor::new(cfds, channel),
        {
            |channel, cfds| {
                let electrum = opts.network.electrum().to_string();
                monitor::Actor::new(electrum, channel, cfds)
            }
        },
        |channel0, channel1| maker_inc_connections::Actor::new(channel0, channel1),
        listener,
        time::Duration::hours(opts.term as i64),
    )
    .await?;

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

pub struct ActorSystem<O, M, T> {
    cfd_actor_addr: Address<maker_cfd::Actor<O, M, T>>,
    cfd_feed_receiver: watch::Receiver<Vec<Cfd>>,
    order_feed_receiver: watch::Receiver<Option<Order>>,
    update_cfd_feed_receiver: watch::Receiver<UpdateCfdProposals>,
}

impl<O, M, T> ActorSystem<O, M, T>
where
    O: xtra::Handler<oracle::MonitorAttestation>
        + xtra::Handler<oracle::GetAnnouncement>
        + xtra::Handler<oracle::FetchAnnouncement>
        + xtra::Handler<oracle::Sync>,
    M: xtra::Handler<monitor::StartMonitoring>
        + xtra::Handler<monitor::Sync>
        + xtra::Handler<monitor::CollaborativeSettlement>
        + xtra::Handler<oracle::Attestation>,
    T: xtra::Handler<maker_inc_connections::TakerMessage>
        + xtra::Handler<maker_inc_connections::BroadcastOrder>
        + xtra::Handler<maker_inc_connections::ListenerMessage>,
{
    #[allow(clippy::too_many_arguments)]
    pub async fn new<F>(
        db: SqlitePool,
        wallet: Wallet,
        oracle_pk: schnorrsig::PublicKey,
        oracle_constructor: impl Fn(Vec<Cfd>, Box<dyn StrongMessageChannel<Attestation>>) -> O,
        monitor_constructor: impl Fn(Box<dyn StrongMessageChannel<monitor::Event>>, Vec<Cfd>) -> F,
        inc_conn_constructor: impl Fn(
            Box<dyn MessageChannel<NewTakerOnline>>,
            Box<dyn MessageChannel<FromTaker>>,
        ) -> T,
        listener: TcpListener,
        term: time::Duration,
    ) -> Result<Self>
    where
        F: Future<Output = Result<M>>,
    {
        let mut conn = db.acquire().await?;

        let cfds = load_all_cfds(&mut conn).await?;

        let (cfd_feed_sender, cfd_feed_receiver) = watch::channel(cfds.clone());
        let (order_feed_sender, order_feed_receiver) = watch::channel::<Option<Order>>(None);
        let (update_cfd_feed_sender, update_cfd_feed_receiver) =
            watch::channel::<UpdateCfdProposals>(HashMap::new());

        let (monitor_addr, mut monitor_ctx) = xtra::Context::new(None);
        let (oracle_addr, mut oracle_ctx) = xtra::Context::new(None);
        let (inc_conn_addr, inc_conn_ctx) = xtra::Context::new(None);

        let cfd_actor_addr = maker_cfd::Actor::new(
            db,
            wallet,
            term,
            oracle_pk,
            cfd_feed_sender,
            order_feed_sender,
            update_cfd_feed_sender,
            inc_conn_addr.clone(),
            monitor_addr.clone(),
            oracle_addr.clone(),
        )
        .create(None)
        .spawn_global();

        tokio::spawn(inc_conn_ctx.run(inc_conn_constructor(
            Box::new(cfd_actor_addr.clone()),
            Box::new(cfd_actor_addr.clone()),
        )));

        tokio::spawn(
            monitor_ctx
                .notify_interval(Duration::from_secs(20), || monitor::Sync)
                .map_err(|e| anyhow::anyhow!(e))?,
        );
        tokio::spawn(
            monitor_ctx
                .run(monitor_constructor(Box::new(cfd_actor_addr.clone()), cfds.clone()).await?),
        );

        tokio::spawn(
            oracle_ctx
                .notify_interval(Duration::from_secs(5), || oracle::Sync)
                .map_err(|e| anyhow::anyhow!(e))?,
        );
        let fan_out_actor = fan_out::Actor::new(&[&cfd_actor_addr, &monitor_addr])
            .create(None)
            .spawn_global();

        tokio::spawn(oracle_ctx.run(oracle_constructor(cfds, Box::new(fan_out_actor))));

        oracle_addr.do_send_async(oracle::Sync).await?;

        let listener_stream = futures::stream::poll_fn(move |ctx| {
            let message = match futures::ready!(listener.poll_accept(ctx)) {
                Ok((stream, address)) => {
                    maker_inc_connections::ListenerMessage::NewConnection { stream, address }
                }
                Err(e) => maker_inc_connections::ListenerMessage::Error { source: e },
            };

            Poll::Ready(Some(message))
        });

        tokio::spawn(inc_conn_addr.attach_stream(listener_stream));

        Ok(Self {
            cfd_actor_addr,
            cfd_feed_receiver,
            order_feed_receiver,
            update_cfd_feed_receiver,
        })
    }
}
