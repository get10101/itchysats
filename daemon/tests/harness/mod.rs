use crate::harness::mocks::oracle::OracleActor;
use crate::harness::mocks::price_feed::PriceFeedActor;
use crate::harness::mocks::wallet::WalletActor;
use crate::schnorrsig;
use ::bdk::bitcoin::Network;
use daemon::auto_rollover;
use daemon::bitmex_price_feed::Quote;
use daemon::connection::connect;
use daemon::connection::ConnectionStatus;
use daemon::db;
use daemon::maker_cfd;
use daemon::model;
use daemon::model::cfd::OrderId;
use daemon::model::cfd::Role;
use daemon::model::FundingRate;
use daemon::model::Identity;
use daemon::model::OpeningFee;
use daemon::model::Price;
use daemon::model::Timestamp;
use daemon::model::Usd;
use daemon::projection;
use daemon::projection::Cfd;
use daemon::projection::CfdOrder;
use daemon::projection::Feeds;
use daemon::seed::RandomSeed;
use daemon::seed::Seed;
use daemon::MakerActorSystem;
use daemon::Tasks;
use daemon::HEARTBEAT_INTERVAL;
use daemon::N_PAYOUTS;
use daemon::SETTLEMENT_INTERVAL;
use rust_decimal_macros::dec;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use xtra::Actor;

pub mod flow;
pub mod maia;
pub mod mocks;

fn oracle_pk() -> schnorrsig::PublicKey {
    schnorrsig::PublicKey::from_str(
        "ddd4636845a90185991826be5a494cde9f4a6947b1727217afedc6292fa4caf7",
    )
    .unwrap()
}

pub async fn start_both() -> (Maker, Taker) {
    let maker = Maker::start(&MakerConfig::default()).await;
    let taker = Taker::start(&TakerConfig::default(), maker.listen_addr, maker.identity).await;
    (maker, taker)
}

pub struct MakerConfig {
    oracle_pk: schnorrsig::PublicKey,
    seed: RandomSeed,
    pub heartbeat_interval: Duration,
    n_payouts: usize,
    dedicated_port: Option<u16>,
}

impl MakerConfig {
    pub fn with_heartbeat_interval(self, interval: Duration) -> Self {
        Self {
            heartbeat_interval: interval,
            ..self
        }
    }

    pub fn with_dedicated_port(self, port: u16) -> Self {
        Self {
            dedicated_port: Some(port),
            ..self
        }
    }
}

impl Default for MakerConfig {
    fn default() -> Self {
        Self {
            oracle_pk: oracle_pk(),
            seed: RandomSeed::default(),
            heartbeat_interval: HEARTBEAT_INTERVAL,
            n_payouts: N_PAYOUTS,
            dedicated_port: None,
        }
    }
}

#[derive(Clone)]
pub struct TakerConfig {
    oracle_pk: schnorrsig::PublicKey,
    seed: RandomSeed,
    pub heartbeat_interval: Duration,
    n_payouts: usize,
}

impl TakerConfig {
    pub fn with_heartbeat_interval(self, timeout: Duration) -> Self {
        Self {
            heartbeat_interval: timeout,
            ..self
        }
    }
}

impl Default for TakerConfig {
    fn default() -> Self {
        Self {
            oracle_pk: oracle_pk(),
            seed: RandomSeed::default(),
            heartbeat_interval: HEARTBEAT_INTERVAL,
            n_payouts: N_PAYOUTS,
        }
    }
}

/// Maker Test Setup
pub struct Maker {
    pub system: MakerActorSystem<OracleActor, WalletActor>,
    pub mocks: mocks::Mocks,
    pub feeds: Feeds,
    pub listen_addr: SocketAddr,
    pub identity: model::Identity,
    _tasks: Tasks,
}

impl Maker {
    pub fn cfd_feed(&mut self) -> &mut watch::Receiver<Vec<Cfd>> {
        &mut self.feeds.cfds
    }

    pub fn order_feed(&mut self) -> &mut watch::Receiver<Option<CfdOrder>> {
        &mut self.feeds.order
    }

    pub fn connected_takers_feed(&mut self) -> &mut watch::Receiver<Vec<Identity>> {
        &mut self.feeds.connected_takers
    }

    pub async fn start(config: &MakerConfig) -> Self {
        let port = match config.dedicated_port {
            Some(port) => port,
            None => {
                // If not explicitly given, get a random free port
                TcpListener::bind("127.0.0.1:0")
                    .await
                    .unwrap()
                    .local_addr()
                    .unwrap()
                    .port()
            }
        };
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);

        let db = db::memory().await.unwrap();

        let mut mocks = mocks::Mocks::default();
        let (oracle, monitor, wallet, price_feed) = mocks::create_actors(&mocks);

        let mut tasks = Tasks::default();

        let (wallet_addr, wallet_fut) = wallet.create(None).run();
        tasks.add(wallet_fut);

        let (price_feed_addr, price_feed_fut) = price_feed.create(None).run();
        tasks.add(price_feed_fut);

        let settlement_interval = SETTLEMENT_INTERVAL;

        let (identity_pk, identity_sk) = config.seed.derive_identity();

        let (projection_actor, projection_context) = xtra::Context::new(None);

        // system startup sends sync messages, mock them
        mocks.mock_sync_handlers().await;

        let maker = daemon::MakerActorSystem::new(
            db.clone(),
            wallet_addr,
            config.oracle_pk,
            |_| oracle,
            |_| Ok(monitor),
            settlement_interval,
            config.n_payouts,
            projection_actor.clone(),
            identity_sk,
            config.heartbeat_interval,
            address,
        )
        .unwrap();

        let (proj_actor, feeds) =
            projection::Actor::new(db, Role::Maker, Network::Testnet, &price_feed_addr);
        tasks.add(projection_context.run(proj_actor));

        Self {
            system: maker,
            feeds,
            identity: model::Identity::new(identity_pk),
            listen_addr: address,
            mocks,
            _tasks: tasks,
        }
    }

    pub async fn publish_order(&mut self, new_order_params: maker_cfd::NewOrder) {
        self.mocks.mock_monitor_oracle_attestation().await;

        self.system
            .cfd_actor
            .send(new_order_params)
            .await
            .unwrap()
            .unwrap();
    }
}

/// Taker Test Setup
pub struct Taker {
    pub id: Identity,
    pub system: daemon::TakerActorSystem<OracleActor, WalletActor, PriceFeedActor>,
    pub mocks: mocks::Mocks,
    pub feeds: Feeds,
    _tasks: Tasks,
}

impl Taker {
    pub fn cfd_feed(&mut self) -> &mut watch::Receiver<Vec<Cfd>> {
        &mut self.feeds.cfds
    }

    pub fn order_feed(&mut self) -> &mut watch::Receiver<Option<CfdOrder>> {
        &mut self.feeds.order
    }

    pub fn quote_feed(&mut self) -> &mut watch::Receiver<Option<projection::Quote>> {
        &mut self.feeds.quote
    }

    pub fn maker_status_feed(&mut self) -> &mut watch::Receiver<ConnectionStatus> {
        &mut self.system.maker_online_status_feed_receiver
    }

    pub async fn start(
        config: &TakerConfig,
        maker_address: SocketAddr,
        maker_identity: model::Identity,
    ) -> Self {
        let (identity_pk, identity_sk) = config.seed.derive_identity();

        let db = db::memory().await.unwrap();

        let mut mocks = mocks::Mocks::default();
        let (oracle, monitor, wallet, price_feed) = mocks::create_actors(&mocks);

        let mut tasks = Tasks::default();

        let (wallet_addr, wallet_fut) = wallet.create(None).run();
        tasks.add(wallet_fut);

        let (projection_actor, projection_context) = xtra::Context::new(None);

        // system startup sends sync messages, mock them
        mocks.mock_sync_handlers().await;

        let taker = daemon::TakerActorSystem::new(
            db.clone(),
            wallet_addr,
            config.oracle_pk,
            identity_sk,
            |_| oracle,
            |_| Ok(monitor),
            move |_| price_feed.clone(),
            config.n_payouts,
            config.heartbeat_interval,
            Duration::from_secs(10),
            projection_actor,
            maker_identity,
        )
        .unwrap();

        let (proj_actor, feeds) =
            projection::Actor::new(db, Role::Taker, Network::Testnet, &taker.price_feed_actor);
        tasks.add(projection_context.run(proj_actor));

        tasks.add(connect(
            taker.maker_online_status_feed_receiver.clone(),
            taker.connection_actor.clone(),
            maker_identity,
            vec![maker_address],
        ));

        Self {
            id: model::Identity::new(identity_pk),
            system: taker,
            feeds,
            mocks,
            _tasks: tasks,
        }
    }

    pub async fn trigger_rollover(&self, id: OrderId) {
        self.system
            .auto_rollover_actor
            .send(auto_rollover::Rollover(id))
            .await
            .unwrap()
    }
}

/// Deliver monitor event to both actor systems
#[macro_export]
macro_rules! deliver_event {
    ($maker:expr, $taker:expr, $event:expr) => {{
        tracing::debug!("Delivering event: {:?}", $event);

        $taker.system.cfd_actor.send($event).await.unwrap();
        $maker.system.cfd_actor.send($event).await.unwrap();
    }};
}

pub fn dummy_price() -> Price {
    Price::new(dec!(50_000)).expect("to not fail")
}

pub fn dummy_quote() -> Quote {
    Quote {
        timestamp: Timestamp::now(),
        bid: dummy_price(),
        ask: dummy_price(),
    }
}

pub fn dummy_new_order() -> maker_cfd::NewOrder {
    maker_cfd::NewOrder {
        price: dummy_price(),
        min_quantity: Usd::new(dec!(5)),
        max_quantity: Usd::new(dec!(100)),
        tx_fee_rate: 1,
        funding_rate: FundingRate::default(),
        opening_fee: OpeningFee::default(),
    }
}

pub fn init_tracing() -> DefaultGuard {
    let filter = EnvFilter::from_default_env()
        // apply warning level globally
        .add_directive(LevelFilter::WARN.into())
        // log traces from test itself
        .add_directive("happy_path=debug".parse().unwrap())
        .add_directive("wire=trace".parse().unwrap())
        .add_directive("taker=debug".parse().unwrap())
        .add_directive("maker=debug".parse().unwrap())
        .add_directive("daemon=debug".parse().unwrap())
        .add_directive("rocket=warn".parse().unwrap());

    let guard = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_test_writer()
        .set_default();

    tracing::info!("Running version: {}", env!("VERGEN_GIT_SEMVER_LIGHTWEIGHT"));

    guard
}
