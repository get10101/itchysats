use crate::harness::mocks::monitor::MonitorActor;
use crate::harness::mocks::oracle::OracleActor;
use crate::harness::mocks::wallet::WalletActor;
use crate::schnorrsig;
use daemon::connection::{Connect, ConnectionStatus};
use daemon::maker_cfd::CfdAction;
use daemon::model::cfd::{Cfd, Order, Origin, UpdateCfdProposals};
use daemon::model::{Price, Usd};
use daemon::seed::Seed;
use daemon::taker_cfd::RollOverParams;
use daemon::{db, maker_cfd, maker_inc_connections, taker_cfd};
use rust_decimal_macros::dec;
use sqlx::SqlitePool;
use std::net::SocketAddr;
use std::str::FromStr;
use std::task::Poll;
use time::Duration;
use tokio::sync::watch;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor;

pub mod bdk;
pub mod flow;
pub mod maia;
pub mod mocks;

pub async fn start_both() -> (Maker, Taker) {
    let oracle_pk: schnorrsig::PublicKey = schnorrsig::PublicKey::from_str(
        "ddd4636845a90185991826be5a494cde9f4a6947b1727217afedc6292fa4caf7",
    )
    .unwrap();

    let rollover_offset = 1;
    let settlement_interval = time::Duration::seconds(1);

    let maker = Maker::start(oracle_pk, settlement_interval).await;
    let taker = Taker::start(
        oracle_pk,
        maker.listen_addr,
        maker.identity_pk,
        settlement_interval,
        rollover_offset,
    )
    .await;
    (maker, taker)
}

const N_PAYOUTS_FOR_TEST: usize = 5;

/// Maker Test Setup
#[derive(Clone)]
pub struct Maker {
    pub cfd_actor_addr: xtra::Address<
        maker_cfd::Actor<OracleActor, MonitorActor, maker_inc_connections::Actor, WalletActor>,
    >,
    pub order_feed: watch::Receiver<Option<Order>>,
    pub cfd_feed: watch::Receiver<Vec<Cfd>>,
    pub update_proposals: watch::Receiver<UpdateCfdProposals>,
    #[allow(dead_code)] // we need to keep the xtra::Address for refcounting
    pub inc_conn_actor_addr: xtra::Address<maker_inc_connections::Actor>,
    pub listen_addr: SocketAddr,
    pub mocks: mocks::Mocks,
    pub identity_pk: x25519_dalek::PublicKey,
}

impl Maker {
    pub async fn start(oracle_pk: schnorrsig::PublicKey, settlement_interval: Duration) -> Self {
        let db = in_memory_db().await;

        let mut mocks = mocks::Mocks::default();
        let (oracle, monitor, wallet) = mocks::create_actors(&mocks);
        mocks.mock_common_empty_handlers().await;

        let wallet_addr = wallet.create(None).spawn_global();

        let seed = Seed::default();
        let (identity_pk, identity_sk) = seed.derive_identity();

        let maker = daemon::MakerActorSystem::new(
            db,
            wallet_addr,
            oracle_pk,
            |_, _| oracle,
            |_, _| async { Ok(monitor) },
            |channel0, channel1| maker_inc_connections::Actor::new(channel0, channel1, identity_sk),
            settlement_interval,
            N_PAYOUTS_FOR_TEST,
        )
        .await
        .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

        let address = listener.local_addr().unwrap();

        let listener_stream = futures::stream::poll_fn(move |ctx| {
            let message = match futures::ready!(listener.poll_accept(ctx)) {
                Ok((stream, address)) => {
                    dbg!("new connection");
                    maker_inc_connections::ListenerMessage::NewConnection { stream, address }
                }
                Err(e) => maker_inc_connections::ListenerMessage::Error { source: e },
            };

            Poll::Ready(Some(message))
        });

        tokio::spawn(maker.inc_conn_addr.clone().attach_stream(listener_stream));

        Self {
            cfd_actor_addr: maker.cfd_actor_addr,
            order_feed: maker.order_feed_receiver,
            cfd_feed: maker.cfd_feed_receiver,
            update_proposals: maker.update_cfd_feed_receiver,
            inc_conn_actor_addr: maker.inc_conn_addr,
            listen_addr: address,
            identity_pk,
            mocks,
        }
    }

    pub async fn publish_order(&mut self, new_order_params: maker_cfd::NewOrder) {
        self.cfd_actor_addr
            .send(new_order_params)
            .await
            .unwrap()
            .unwrap();
    }

    pub async fn reject_take_request(&self, order: Order) {
        self.cfd_actor_addr
            .send(CfdAction::RejectOrder { order_id: order.id })
            .await
            .unwrap()
            .unwrap();
    }

    pub async fn accept_take_request(&self, order: Order) {
        self.cfd_actor_addr
            .send(CfdAction::AcceptOrder { order_id: order.id })
            .await
            .unwrap()
            .unwrap();
    }

    pub async fn accept_rollover(&self, order: Order) {
        self.cfd_actor_addr
            .send(CfdAction::AcceptRollOver { order_id: order.id })
            .await
            .unwrap()
            .unwrap();
    }
}

/// Taker Test Setup
#[derive(Clone)]
pub struct Taker {
    pub order_feed: watch::Receiver<Option<Order>>,
    pub cfd_feed: watch::Receiver<Vec<Cfd>>,
    pub maker_status_feed: watch::Receiver<ConnectionStatus>,
    pub cfd_actor_addr: xtra::Address<taker_cfd::Actor<OracleActor, MonitorActor, WalletActor>>,
    pub update_proposals: watch::Receiver<UpdateCfdProposals>,
    pub mocks: mocks::Mocks,
}

impl Taker {
    pub async fn start(
        oracle_pk: schnorrsig::PublicKey,
        maker_address: SocketAddr,
        maker_noise_pub_key: x25519_dalek::PublicKey,
        settlement_interval: Duration,
        rollover_offset: u32,
    ) -> Self {
        let seed = Seed::default();

        let (_, identity_sk) = seed.derive_identity();

        let db = in_memory_db().await;

        let mut mocks = mocks::Mocks::default();
        let (oracle, monitor, wallet) = mocks::create_actors(&mocks);
        mocks.mock_common_empty_handlers().await;

        let wallet_addr = wallet.create(None).spawn_global();

        let taker = daemon::TakerActorSystem::new(
            db,
            wallet_addr,
            oracle_pk,
            identity_sk,
            |_, _| oracle,
            |_, _| async { Ok(monitor) },
            RollOverParams::new(rollover_offset, settlement_interval).unwrap(),
            N_PAYOUTS_FOR_TEST,
        )
        .await
        .unwrap();

        taker
            .connection_actor_addr
            .send(Connect {
                maker_identity_pk: maker_noise_pub_key,
                maker_addr: maker_address,
            })
            .await
            .unwrap()
            .unwrap();

        Self {
            order_feed: taker.order_feed_receiver,
            cfd_feed: taker.cfd_feed_receiver,
            maker_status_feed: taker.maker_online_status_feed_receiver,
            cfd_actor_addr: taker.cfd_actor_addr,
            update_proposals: taker.update_cfd_feed_receiver,
            mocks,
        }
    }

    pub async fn take_order(&self, order: Order, quantity: Usd) {
        self.cfd_actor_addr
            .send(taker_cfd::TakeOffer {
                order_id: order.id,
                quantity,
            })
            .await
            .unwrap()
            .unwrap();
    }

    pub async fn trigger_auto_rollover(&self) {
        self.cfd_actor_addr
            .send(taker_cfd::AutoRollover)
            .await
            .unwrap();
    }
}

async fn in_memory_db() -> SqlitePool {
    // Note: Every :memory: database is distinct from every other. So, opening two database
    // connections each with the filename ":memory:" will create two independent in-memory
    // databases. see: https://www.sqlite.org/inmemorydb.html
    let pool = SqlitePool::connect(":memory:").await.unwrap();

    db::run_migrations(&pool).await.unwrap();

    pool
}

/// The order cannot be directly compared in tests as the origin is different,
/// therefore wrap the assertion macro in a code that unifies the 'Origin'
pub fn assert_is_same_order(a: &Order, b: &Order) {
    // Assume the same origin
    let mut a = a.clone();
    let mut b = b.clone();
    a.origin = Origin::Ours;
    b.origin = Origin::Ours;

    assert_eq!(a, b);
}

pub fn dummy_new_order() -> maker_cfd::NewOrder {
    maker_cfd::NewOrder {
        price: Price::new(dec!(50_000)).expect("unexpected failure"),
        min_quantity: Usd::new(dec!(5)),
        max_quantity: Usd::new(dec!(100)),
    }
}

pub fn init_tracing() -> DefaultGuard {
    let filter = EnvFilter::from_default_env()
        // apply warning level globally
        .add_directive(format!("{}", LevelFilter::WARN).parse().unwrap())
        // log traces from test itself
        .add_directive(
            format!("happy_path={}", LevelFilter::DEBUG)
                .parse()
                .unwrap(),
        )
        .add_directive(format!("taker={}", LevelFilter::DEBUG).parse().unwrap())
        .add_directive(format!("maker={}", LevelFilter::DEBUG).parse().unwrap())
        .add_directive(format!("daemon={}", LevelFilter::DEBUG).parse().unwrap())
        .add_directive(format!("rocket={}", LevelFilter::WARN).parse().unwrap());

    let guard = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_test_writer()
        .set_default();

    tracing::info!("Running version: {}", env!("VERGEN_GIT_SEMVER_LIGHTWEIGHT"));

    guard
}
