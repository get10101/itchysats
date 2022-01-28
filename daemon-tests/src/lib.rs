use crate::mocks::monitor::MonitorActor;
use crate::mocks::oracle::OracleActor;
use crate::mocks::price_feed::PriceFeedActor;
use crate::mocks::wallet::WalletActor;
use daemon::auto_rollover;
use daemon::bdk::bitcoin::secp256k1::schnorrsig;
use daemon::bdk::bitcoin::Amount;
use daemon::bdk::bitcoin::Network;
use daemon::connection::connect;
use daemon::connection::ConnectionStatus;
use daemon::db;
use daemon::maker_cfd;
use daemon::projection;
use daemon::projection::Cfd;
use daemon::projection::CfdOrder;
use daemon::projection::Feeds;
use daemon::seed::RandomSeed;
use daemon::seed::Seed;
use daemon::MakerActorSystem;
use daemon::HEARTBEAT_INTERVAL;
use daemon::N_PAYOUTS;
use model::FundingRate;
use model::Identity;
use model::OpeningFee;
use model::OrderId;
use model::Position;
use model::Price;
use model::Role;
use model::TxFeeRate;
use model::Usd;
use model::SETTLEMENT_INTERVAL;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_tasks::Tasks;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use xtra::Actor;
use xtra_bitmex_price_feed::Quote;

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

#[derive(Clone, Copy)]
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

#[derive(Clone, Copy)]
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

        let (wallet, wallet_mock) = WalletActor::new();
        let (price_feed, price_feed_mock) = PriceFeedActor::new();

        let mut tasks = Tasks::default();

        let (wallet_addr, wallet_fut) = wallet.create(None).run();
        tasks.add(wallet_fut);

        let (price_feed_addr, price_feed_fut) = price_feed.create(None).run();
        tasks.add(async move {
            let _ = price_feed_fut.await;
        });

        let settlement_interval = SETTLEMENT_INTERVAL;

        let (identity_pk, identity_sk) = config.seed.derive_identity();

        let (projection_actor, projection_context) = xtra::Context::new(None);

        let mut monitor_mock = None;
        let mut oracle_mock = None;

        let maker = daemon::MakerActorSystem::new(
            db.clone(),
            wallet_addr,
            config.oracle_pk,
            |executor| {
                let (oracle, mock) = OracleActor::new(executor);
                oracle_mock = Some(mock);

                oracle
            },
            |executor| {
                let (monitor, mock) = MonitorActor::new(executor);
                monitor_mock = Some(mock);

                Ok(monitor)
            },
            settlement_interval,
            config.n_payouts,
            projection_actor,
            identity_sk,
            config.heartbeat_interval,
            address,
        )
        .unwrap();

        let mocks = mocks::Mocks::new(
            wallet_mock,
            price_feed_mock,
            monitor_mock.unwrap(),
            oracle_mock.unwrap(),
        );

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

    pub async fn set_offer_params(&mut self, offer_params: maker_cfd::OfferParams) {
        let maker_cfd::OfferParams {
            price,
            min_quantity,
            max_quantity,
            tx_fee_rate,
            funding_rate,
            opening_fee,
            position,
        } = offer_params;
        self.system
            .set_offer_params(
                price,
                min_quantity,
                max_quantity,
                Some(tx_fee_rate),
                Some(funding_rate),
                Some(opening_fee),
                Some(position),
            )
            .await
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

        let mut tasks = Tasks::default();

        let (wallet, wallet_mock) = WalletActor::new();
        let (price_feed, price_feed_mock) = PriceFeedActor::new();

        let (wallet_addr, wallet_fut) = wallet.create(None).run();
        tasks.add(wallet_fut);

        let (projection_actor, projection_context) = xtra::Context::new(None);

        let mut oracle_mock = None;
        let mut monitor_mock = None;

        let taker = daemon::TakerActorSystem::new(
            db.clone(),
            wallet_addr,
            config.oracle_pk,
            identity_sk,
            |executor| {
                let (oracle, mock) = OracleActor::new(executor);
                oracle_mock = Some(mock);

                oracle
            },
            |executor| {
                let (monitor, mock) = MonitorActor::new(executor);
                monitor_mock = Some(mock);

                Ok(monitor)
            },
            move || price_feed.clone(),
            config.n_payouts,
            config.heartbeat_interval,
            Duration::from_secs(10),
            projection_actor,
            maker_identity,
        )
        .unwrap();

        let mocks = mocks::Mocks::new(
            wallet_mock,
            price_feed_mock,
            monitor_mock.unwrap(),
            oracle_mock.unwrap(),
        );

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

/// Simulate oracle attestation for both actor systems
#[macro_export]
macro_rules! simulate_attestation {
    ($maker:expr, $taker:expr, $order_id:expr, $attestation:expr) => {{
        tracing::debug!("Simulating attestation: {:?}", $attestation);

        $maker
            .mocks
            .oracle()
            .await
            .simulate_attestation($order_id, $attestation)
            .await;

        $taker
            .mocks
            .oracle()
            .await
            .simulate_attestation($order_id, $attestation)
            .await;
    }};
}

/// Waits until the CFDs for both maker and taker are in the given state.
#[macro_export]
macro_rules! wait_next_state {
    ($id:expr, $maker:expr, $taker:expr, $maker_state:expr, $taker_state:expr) => {
        let wait_until_taker = next_with($taker.cfd_feed(), one_cfd_with_state($taker_state));
        let wait_until_maker = next_with($maker.cfd_feed(), one_cfd_with_state($maker_state));

        let (taker_cfd, maker_cfd) = tokio::join!(wait_until_taker, wait_until_maker);
        let taker_cfd = taker_cfd.unwrap();
        let maker_cfd = maker_cfd.unwrap();

        assert_eq!(
            taker_cfd.order_id, maker_cfd.order_id,
            "order id mismatch between maker and taker"
        );
        assert_eq!(taker_cfd.order_id, $id, "unexpected order id in the taker");
        assert_eq!(maker_cfd.order_id, $id, "unexpected order id in the maker");
    };
    ($id:expr, $maker:expr, $taker:expr, $state:expr) => {
        wait_next_state!($id, $maker, $taker, $state, $state)
    };
}

pub fn dummy_quote() -> Quote {
    Quote {
        timestamp: OffsetDateTime::now_utc(),
        bid: dummy_price(),
        ask: dummy_price(),
    }
}

pub fn dummy_offer_params(position: Position) -> maker_cfd::OfferParams {
    maker_cfd::OfferParams {
        price: Price::new(dummy_price()).unwrap(),
        min_quantity: Usd::new(dec!(5)),
        max_quantity: Usd::new(dec!(100)),
        tx_fee_rate: TxFeeRate::new(1),
        // 8.76% annualized = rate of 0.0876 annualized = rate of 0.00024 daily
        funding_rate: FundingRate::new(dec!(0.00024)).unwrap(),
        opening_fee: OpeningFee::new(Amount::from_sat(2)),
        position,
    }
}

fn dummy_price() -> Decimal {
    dec!(50_000)
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

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_test_writer()
        .set_default()
}
