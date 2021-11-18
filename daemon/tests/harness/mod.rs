use crate::harness::flow::{is_next_none, next, next_cfd, next_order};
use crate::harness::mocks::monitor::MonitorActor;
use crate::harness::mocks::oracle::OracleActor;
use crate::harness::mocks::wallet::WalletActor;
use crate::schnorrsig;
use anyhow::Result;
use daemon::connection::{Connect, ConnectionStatus};
use daemon::maker_cfd::CfdAction;
use daemon::model::cfd::{Cfd, Order, OrderId, Origin, RollOverProposal, UpdateCfdProposal};
use daemon::model::{Price, Timestamp, Usd};
use daemon::seed::Seed;
use daemon::{db, maker_cfd, maker_inc_connections, monitor, taker_cfd, MakerActorSystem, Tasks};
use rust_decimal_macros::dec;
use sqlx::SqlitePool;
use std::net::SocketAddr;
use std::str::FromStr;
use std::task::Poll;
use std::time::Duration;
use tokio::sync::watch;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use xtra::Actor;

pub mod bdk;
pub mod flow;
pub mod maia;
pub mod mocks;

pub const HEARTBEAT_INTERVAL_FOR_TEST: Duration = Duration::from_secs(2);
const N_PAYOUTS_FOR_TEST: usize = 5;

pub async fn start_both() -> (Maker, Taker) {
    let oracle_pk: schnorrsig::PublicKey = schnorrsig::PublicKey::from_str(
        "ddd4636845a90185991826be5a494cde9f4a6947b1727217afedc6292fa4caf7",
    )
    .unwrap();

    let maker = Maker::start(oracle_pk).await;
    let taker = Taker::start(oracle_pk, maker.listen_addr, maker.identity_pk).await;
    (maker, taker)
}

pub async fn init_open_cfd(maker: &mut Maker, taker: &mut Taker) -> Result<(Cfd, Cfd)> {
    is_next_none(taker.order_feed()).await.unwrap();

    maker.publish_order(dummy_new_order()).await;

    let (_, received) = next_order(maker.order_feed(), taker.order_feed())
        .await
        .unwrap();

    taker.take_order(received.clone(), Usd::new(dec!(5))).await;
    let (_, _) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();

    maker.accept_take_request(received.clone()).await;

    next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();

    maker
        .system
        .cfd_actor_addr
        .send(monitor::Event::LockFinality(maker_cfd.order.id))
        .await
        .unwrap();
    taker
        .system
        .cfd_actor_addr
        .send(monitor::Event::LockFinality(taker_cfd.order.id))
        .await
        .unwrap();

    Ok(next_cfd(&mut taker.cfd_feed(), &mut maker.cfd_feed()).await?)
}

/// Maker Test Setup
pub struct Maker {
    pub system:
        MakerActorSystem<OracleActor, MonitorActor, maker_inc_connections::Actor, WalletActor>,
    pub mocks: mocks::Mocks,
    pub listen_addr: SocketAddr,
    pub identity_pk: x25519_dalek::PublicKey,
    _tasks: Tasks,
}

impl Maker {
    pub fn cfd_feed(&mut self) -> &mut watch::Receiver<Vec<Cfd>> {
        &mut self.system.cfd_feed_receiver
    }

    pub fn order_feed(&mut self) -> &mut watch::Receiver<Option<Order>> {
        &mut self.system.order_feed_receiver
    }

    pub async fn start(oracle_pk: schnorrsig::PublicKey) -> Self {
        let db = in_memory_db().await;

        let mut mocks = mocks::Mocks::default();
        let (oracle, monitor, wallet) = mocks::create_actors(&mocks);

        let mut tasks = Tasks::default();

        let (wallet_addr, wallet_fut) = wallet.create(None).run();
        tasks.add(wallet_fut);

        let settlement_interval = time::Duration::hours(24);

        let seed = Seed::default();
        let (identity_pk, identity_sk) = seed.derive_identity();

        // system startup sends sync messages, mock them
        mocks.mock_sync_handlers().await;
        mocks.mock_rollover_handlers().await;

        let maker = daemon::MakerActorSystem::new(
            db,
            wallet_addr,
            oracle_pk,
            |_, _| oracle,
            |_, _| async { Ok(monitor) },
            |channel0, channel1| {
                maker_inc_connections::Actor::new(
                    channel0,
                    channel1,
                    identity_sk,
                    HEARTBEAT_INTERVAL_FOR_TEST,
                )
            },
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
                    maker_inc_connections::ListenerMessage::NewConnection { stream, address }
                }
                Err(e) => maker_inc_connections::ListenerMessage::Error { source: e },
            };

            Poll::Ready(Some(message))
        });

        tasks.add(maker.inc_conn_addr.clone().attach_stream(listener_stream));

        Self {
            system: maker,
            identity_pk,
            listen_addr: address,
            mocks,
            _tasks: tasks,
        }
    }

    pub async fn publish_order(&mut self, new_order_params: maker_cfd::NewOrder) {
        self.mocks.mock_monitor_oracle_attestation().await;

        self.system
            .cfd_actor_addr
            .send(new_order_params)
            .await
            .unwrap()
            .unwrap();
    }

    pub async fn reject_take_request(&self, order: Order) {
        self.system
            .cfd_actor_addr
            .send(CfdAction::RejectOrder { order_id: order.id })
            .await
            .unwrap()
            .unwrap();
    }

    pub async fn accept_take_request(&self, order: Order) {
        self.system
            .cfd_actor_addr
            .send(CfdAction::AcceptOrder { order_id: order.id })
            .await
            .unwrap()
            .unwrap();
    }

    pub async fn accept_rollover(&self, order: Order) {
        self.system
            .cfd_actor_addr
            .send(CfdAction::AcceptRollOver { order_id: order.id })
            .await
            .unwrap()
            .unwrap();
    }

    pub async fn get_update_cfd_proposal(
        &mut self,
        order_id: &OrderId,
    ) -> Result<UpdateCfdProposal> {
        tokio::time::timeout(tokio::time::Duration::from_secs(1), async {
            loop {
                let update_proposals = next(&mut self.system.update_cfd_feed_receiver).await?;
                if let Some(update_proposal) = update_proposals.get(order_id) {
                    return Ok(update_proposal.clone());
                }
            }
        })
        .await?
    }
}

/// Taker Test Setup
pub struct Taker {
    pub system: daemon::TakerActorSystem<OracleActor, MonitorActor, WalletActor>,
    pub mocks: mocks::Mocks,
    _tasks: Tasks,
}

impl Taker {
    pub fn cfd_feed(&mut self) -> &mut watch::Receiver<Vec<Cfd>> {
        &mut self.system.cfd_feed_receiver
    }

    pub fn order_feed(&mut self) -> &mut watch::Receiver<Option<Order>> {
        &mut self.system.order_feed_receiver
    }

    pub fn maker_status_feed(&mut self) -> &mut watch::Receiver<ConnectionStatus> {
        &mut self.system.maker_online_status_feed_receiver
    }

    pub async fn start(
        oracle_pk: schnorrsig::PublicKey,
        maker_address: SocketAddr,
        maker_noise_pub_key: x25519_dalek::PublicKey,
    ) -> Self {
        let seed = Seed::default();

        let (_, identity_sk) = seed.derive_identity();

        let db = in_memory_db().await;

        let mut mocks = mocks::Mocks::default();
        let (oracle, monitor, wallet) = mocks::create_actors(&mocks);

        let mut tasks = Tasks::default();

        let (wallet_addr, wallet_fut) = wallet.create(None).run();
        tasks.add(wallet_fut);

        // system startup sends sync messages, mock them
        mocks.mock_sync_handlers().await;
        mocks.mock_rollover_handlers().await;

        let taker = daemon::TakerActorSystem::new(
            db,
            wallet_addr,
            oracle_pk,
            identity_sk,
            |_, _| oracle,
            |_, _| async { Ok(monitor) },
            N_PAYOUTS_FOR_TEST,
            HEARTBEAT_INTERVAL_FOR_TEST * 2,
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
            system: taker,
            mocks,
            _tasks: tasks,
        }
    }

    pub async fn take_order(&self, order: Order, quantity: Usd) {
        self.system
            .cfd_actor_addr
            .send(taker_cfd::TakeOffer {
                order_id: order.id,
                quantity,
            })
            .await
            .unwrap()
            .unwrap();
    }

    pub async fn trigger_propose_rollover(&self, order_id: OrderId) {
        self.system
            .cfd_actor_addr
            .send(taker_cfd::ProposeRollOver {
                proposal: RollOverProposal {
                    order_id,
                    timestamp: Timestamp::now().expect("able to get current timestamp"),
                },
            })
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
