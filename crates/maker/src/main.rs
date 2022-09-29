use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use daemon::bdk::FeeRate;
use daemon::monitor;
use daemon::oracle;
use daemon::projection;
use daemon::seed::RandomSeed;
use daemon::seed::Seed;
use daemon::wallet;
use daemon::wallet::MAKER_WALLET_ID;
use daemon::N_PAYOUTS;
use maker::load_blocked_peers;
use maker::routes;
use maker::ActorSystem;
use maker::Opts;
use model::olivia;
use model::Role;
use model::SETTLEMENT_INTERVAL;
use rocket_cookie_auth::users::Users;
use shared_bin::catchers::default_catchers;
use shared_bin::cli::Withdraw;
use shared_bin::fairings;
use shared_bin::logger;
use std::net::SocketAddr;
use tokio_extras::Tasks;
use xtras::supervisor::always_restart;
use xtras::supervisor::Supervisor;

#[rocket::main]
async fn main() -> Result<()> {
    let opts = Opts::parse();

    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("unable to get cwd"));

    let data_dir = opts.network.data_dir(data_dir);

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

    let seed = RandomSeed::initialize(&data_dir.join("maker_seed")).await?;

    let bitcoin_network = opts.network.bitcoin_network();

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

    wallet_dir.push(MAKER_WALLET_ID);
    let (wallet, wallet_feed_receiver) =
        wallet::Actor::spawn(opts.network.electrum(), ext_priv_key, wallet_dir)?;

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

    let identities = seed.derive_identities();

    let peer_id = identities.peer_id();
    let hex_pk = hex::encode(identities.identity_pk.to_bytes());
    tracing::info!("Connection details: maker_id='{hex_pk}', peer_id='{peer_id}'");

    let figment = rocket::Config::figment()
        .merge(("address", opts.http_address.ip()))
        .merge(("port", opts.http_address.port()))
        .merge(("cli_colors", false))
        .merge(("secret_key", RandomSeed::default().seed()));

    let p2p_port = opts.p2p_port;
    let p2p_socket = format!("0.0.0.0:{p2p_port}").parse::<SocketAddr>().unwrap();

    let db =
        sqlite_db::connect(data_dir.join("maker.sqlite"), opts.ignore_migration_errors).await?;

    let blocked_peers = load_blocked_peers(&data_dir)
        .await
        .context("Failed to load blocked peers")?;

    // Create actors
    let endpoint_listen =
        daemon::libp2p_utils::create_listen_tcp_multiaddr(&p2p_socket.ip(), p2p_socket.port())
            .expect("to parse properly");

    let (supervisor, price_feed) = Supervisor::with_policy(
        {
            let network = opts.network.bitmex_network();
            move || xtra_bitmex_price_feed::Actor::new(network)
        },
        always_restart::<xtra_bitmex_price_feed::Error>(),
    );
    tasks.add(supervisor.run_log_summary());

    let (feed_senders, feed_receivers) = projection::feeds();
    let feed_senders = std::sync::Arc::new(feed_senders);

    let (supervisor, projection_actor) = Supervisor::new({
        let db = db.clone();
        move || {
            projection::Actor::new(
                db.clone(),
                bitcoin_network,
                price_feed.clone().into(),
                Role::Maker,
                feed_senders.clone(),
            )
        }
    });
    tasks.add(supervisor.run_log_summary());

    let maker = ActorSystem::new(
        db.clone(),
        wallet.clone(),
        *olivia::PUBLIC_KEY,
        |executor| oracle::Actor::new(db.clone(), executor),
        |executor| {
            let electrum = opts.network.electrum().to_string();
            monitor::Actor::new(db.clone(), electrum, executor)
        },
        SETTLEMENT_INTERVAL,
        N_PAYOUTS,
        projection_actor.clone(),
        identities,
        endpoint_listen,
        blocked_peers,
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

    let mission_success = rocket::custom(figment)
        .manage(feed_receivers)
        .manage(wallet_feed_receiver)
        .manage(maker)
        .manage(users)
        .manage(bitcoin_network)
        .mount(
            "/api",
            rocket::routes![
                routes::maker_feed,
                routes::put_offer_params,
                routes::put_offer_params_for_symbol,
                routes::post_cfd_action,
                routes::get_cfds,
                routes::put_sync_wallet,
                shared_bin::routes::get_health_check,
                shared_bin::routes::get_metrics,
                shared_bin::routes::get_version,
                shared_bin::routes::change_password,
                shared_bin::routes::logout,
                shared_bin::routes::is_authenticated,
                shared_bin::routes::post_login,
            ],
        )
        .register("/api", default_catchers())
        .mount("/", rocket::routes![routes::dist, routes::index])
        .register("/", default_catchers())
        .attach(fairings::log_launch())
        .attach(fairings::log_requests())
        .attach(fairings::ui_browser_launch(!opts.headless))
        .launch()
        .await?;

    tracing::trace!(?mission_success, "Rocket has landed");

    db.close().await;

    Ok(())
}

struct RocketAuthDbConnection {
    inner: sqlite_db::Connection,
}

impl RocketAuthDbConnection {
    fn new(db: sqlite_db::Connection) -> Self {
        Self { inner: db }
    }
}

#[rocket::async_trait]
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
