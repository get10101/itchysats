use anyhow::{Context, Result};
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::{ecdsa, Txid};
use cfd_protocol::secp256k1_zkp::{schnorrsig, Secp256k1};
use cfd_protocol::PartyParams;
use daemon::maker_cfd::CfdAction;
use daemon::model::cfd::{Cfd, CfdState, Order};
use daemon::model::{Price, Timestamp, Usd, WalletInfo};
use daemon::tokio_ext::FutureExt;
use daemon::{
    connection, db, maker_cfd, maker_inc_connections, monitor, oracle, taker_cfd, wallet,
};

use mockall::predicate::*;
use mockall::*;
use rand::thread_rng;
use rust_decimal_macros::dec;
use sqlx::SqlitePool;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use tokio::sync::{watch, Mutex};
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use xtra::prelude::*;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra_productivity::xtra_productivity;

#[tokio::test]
async fn taker_receives_order_from_maker_on_publication() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    assert!(is_next_none(&mut taker.order_feed).await);

    // POC of the call
    // does not mean anything in the test, but this is how it would look like
    maker
        .mocks
        .wallet
        .lock()
        .await
        .expect_sync()
        .times(1)
        .returning(|| dummy_wallet_info());

    // Uncomment this to showcase that we can assert that a particular caller
    // got called (with certain parameters too)
    // maker.mocks.wallet.lock().await.checkpoint();

    maker.publish_order(new_dummy_order());

    let (published, received) = tokio::join!(
        next_some(&mut maker.order_feed),
        next_some(&mut taker.order_feed)
    );

    // TODO: Add assertion function so we can assert on the other order values
    assert_eq!(published.id, received.id);
}

#[tokio::test]
async fn taker_takes_order_and_maker_rejects() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    // TODO: Why is this needed? For the cfd stream it is not needed
    is_next_none(&mut taker.order_feed).await;

    maker.publish_order(new_dummy_order());

    let (_, received) = next_order(&mut maker.order_feed, &mut taker.order_feed).await;

    taker.take_order(received.clone(), Usd::new(dec!(10)));

    let (taker_cfd, maker_cfd) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);
    assert!(matches!(
        taker_cfd.state,
        CfdState::OutgoingOrderRequest { .. }
    ));
    assert!(matches!(
        maker_cfd.state,
        CfdState::IncomingOrderRequest { .. }
    ));

    maker.reject_take_request(received.clone());

    let (taker_cfd, maker_cfd) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;
    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::Rejected { .. }));
    assert!(matches!(maker_cfd.state, CfdState::Rejected { .. }));
}

fn new_dummy_order() -> maker_cfd::NewOrder {
    maker_cfd::NewOrder {
        price: Price::new(dec!(50_000)).expect("unexpected failure"),
        min_quantity: Usd::new(dec!(10)),
        max_quantity: Usd::new(dec!(100)),
    }
}

/// Returns the first `Cfd` from both channels
///
/// Ensures that there is only one `Cfd` present in both channels.
async fn next_cfd(
    rx_a: &mut watch::Receiver<Vec<Cfd>>,
    rx_b: &mut watch::Receiver<Vec<Cfd>>,
) -> (Cfd, Cfd) {
    let (a, b) = tokio::join!(next(rx_a), next(rx_b));

    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);

    (a.first().unwrap().clone(), b.first().unwrap().clone())
}

async fn next_order(
    rx_a: &mut watch::Receiver<Option<Order>>,
    rx_b: &mut watch::Receiver<Option<Order>>,
) -> (Order, Order) {
    let (a, b) = tokio::join!(next_some(rx_a), next_some(rx_b));

    (a, b)
}

/// Returns the value if the next Option received on the stream is Some
///
/// Panics if None is received on the stream.
async fn next_some<T>(rx: &mut watch::Receiver<Option<T>>) -> T
where
    T: Clone,
{
    if let Some(value) = next(rx).await {
        value
    } else {
        panic!("Received None when Some was expected")
    }
}

/// Returns true if the next Option received on the stream is None
///
/// Returns false if Some is received.
async fn is_next_none<T>(rx: &mut watch::Receiver<Option<T>>) -> bool
where
    T: Clone,
{
    next(rx).await.is_none()
}

/// Returns watch channel value upon change
async fn next<T>(rx: &mut watch::Receiver<T>) -> T
where
    T: Clone,
{
    rx.changed()
        .timeout(Duration::from_secs(5))
        .await
        .context("Waiting for next element in channel is taking too long, aborting")
        .unwrap()
        .unwrap();
    rx.borrow().clone()
}

fn init_tracing() -> DefaultGuard {
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

/// Test Stub simulating the Oracle actor

struct OracleActor {
    mock: Arc<Mutex<dyn Oracle + Send>>,
}
impl xtra::Actor for OracleActor {}
impl Oracle for OracleActor {}

#[xtra_productivity(message_impl = false)]
impl OracleActor {
    async fn handle(&mut self, _msg: oracle::GetAnnouncement) -> Option<oracle::Announcement> {
        self.mock.lock().await.get_announcement()
    }

    async fn handle(&mut self, _msg: oracle::MonitorAttestation) {
        self.mock.lock().await.monitor_attestation()
    }

    async fn handle(&mut self, _msg: oracle::Sync) {
        self.mock.lock().await.sync()
    }
}

#[automock]
trait Oracle {
    fn get_announcement(&mut self) -> Option<oracle::Announcement> {
        todo!("stub this if needed")
    }

    fn monitor_attestation(&mut self) {
        todo!("stub this if needed")
    }

    fn sync(&mut self) {}
}

/// Test Stub simulating the Monitor actor
struct MonitorActor {
    mock: Arc<Mutex<dyn Monitor + Send>>,
}

impl xtra::Actor for MonitorActor {}
impl Monitor for MonitorActor {}

#[xtra_productivity(message_impl = false)]
impl MonitorActor {
    async fn handle(&mut self, _msg: monitor::Sync) {
        self.mock.lock().await.sync()
    }

    async fn handle(&mut self, _msg: monitor::StartMonitoring) {
        self.mock.lock().await.start_monitoring()
    }

    async fn handle(&mut self, _msg: monitor::CollaborativeSettlement) {
        self.mock.lock().await.collaborative_settlement()
    }

    async fn handle(&mut self, _msg: oracle::Attestation) {
        self.mock.lock().await.oracle_attestation()
    }
}

#[automock]
trait Monitor {
    fn sync(&mut self) {
        tracing::debug!("Issuing a monitor sync message")
    }

    fn start_monitoring(&mut self) {
        todo!("stub this if needed")
    }

    fn collaborative_settlement(&mut self) {
        todo!("stub this if needed")
    }

    fn oracle_attestation(&mut self) {
        todo!("stub this if needed")
    }
}

/// Test Stub simulating the Wallet actor
/// Serves as an entrypoint for injected handlers
struct WalletActor {
    mock: Arc<Mutex<dyn Wallet + Send>>,
}

impl xtra::Actor for WalletActor {}

#[xtra_productivity(message_impl = false)]
impl WalletActor {
    async fn handle(&mut self, _msg: wallet::BuildPartyParams) -> Result<PartyParams> {
        self.mock.lock().await.build_party_params()
    }
    async fn handle(&mut self, _msg: wallet::Sync) -> Result<WalletInfo> {
        tracing::info!("We are handling the wallet sync msg");
        self.mock.lock().await.sync()
    }
    async fn handle(&mut self, _msg: wallet::Sign) -> Result<PartiallySignedTransaction> {
        self.mock.lock().await.sign()
    }
    async fn handle(&mut self, _msg: wallet::TryBroadcastTransaction) -> Result<Txid> {
        self.mock.lock().await.broadcast()
    }
}

#[automock]
trait Wallet {
    fn build_party_params(&mut self) -> Result<PartyParams> {
        todo!()
    }

    fn sign(&mut self) -> Result<PartiallySignedTransaction> {
        todo!()
    }

    fn broadcast(&mut self) -> Result<Txid> {
        todo!()
    }

    fn sync(&mut self) -> Result<WalletInfo> {
        dummy_wallet_info()
    }
}

#[derive(Clone)]
struct Mocks {
    wallet: Arc<Mutex<MockWallet>>,
    monitor: Arc<Mutex<MockMonitor>>,
    oracle: Arc<Mutex<MockOracle>>,
}

impl Default for Mocks {
    fn default() -> Self {
        Self {
            oracle: Arc::new(Mutex::new(MockOracle::new())),
            monitor: Arc::new(Mutex::new(MockMonitor::new())),
            wallet: Arc::new(Mutex::new(MockWallet::new())),
        }
    }
}

/// Maker Test Setup
#[derive(Clone)]
struct Maker {
    cfd_actor_addr: xtra::Address<
        maker_cfd::Actor<OracleActor, MonitorActor, maker_inc_connections::Actor, WalletActor>,
    >,
    order_feed: watch::Receiver<Option<Order>>,
    cfd_feed: watch::Receiver<Vec<Cfd>>,
    #[allow(dead_code)] // we need to keep the xtra::Address for refcounting
    inc_conn_actor_addr: xtra::Address<maker_inc_connections::Actor>,
    listen_addr: SocketAddr,
    mocks: Mocks,
}

struct SimpleWallet {}

impl Wallet for SimpleWallet {}

fn dummy_wallet_info() -> Result<WalletInfo> {
    let s = Secp256k1::new();
    let public_key = ecdsa::PublicKey::new(s.generate_keypair(&mut thread_rng()).1);
    let address = bdk::bitcoin::Address::p2pkh(&public_key, bdk::bitcoin::Network::Testnet);

    Ok(WalletInfo {
        balance: bdk::bitcoin::Amount::ONE_BTC,
        address,
        last_updated_at: Timestamp::now()?,
    })
}

/// Creates actors with embedded mock handlers
fn create_actors(mocks: &Mocks) -> (OracleActor, MonitorActor, WalletActor) {
    let oracle = OracleActor {
        mock: mocks.oracle.clone(),
    };
    let monitor = MonitorActor {
        mock: mocks.monitor.clone(),
    };
    let wallet = WalletActor {
        mock: mocks.wallet.clone(),
    };
    (oracle, monitor, wallet)
}

impl Maker {
    async fn start(oracle_pk: schnorrsig::PublicKey) -> Self {
        let db = in_memory_db().await;

        let mocks = Mocks::default();
        let (oracle, monitor, wallet) = create_actors(&mocks);

        let wallet_addr = wallet.create(None).spawn_global();

        let settlement_time_interval_hours = time::Duration::hours(24);

        let maker = daemon::MakerActorSystem::new(
            db,
            wallet_addr,
            oracle_pk,
            |_, _| oracle,
            |_, _| async { Ok(monitor) },
            |channel0, channel1| maker_inc_connections::Actor::new(channel0, channel1),
            settlement_time_interval_hours,
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
            inc_conn_actor_addr: maker.inc_conn_addr,
            listen_addr: address,
            mocks,
        }
    }

    fn publish_order(&mut self, new_order_params: maker_cfd::NewOrder) {
        self.cfd_actor_addr.do_send(new_order_params).unwrap();
    }

    fn reject_take_request(&self, order: Order) {
        self.cfd_actor_addr
            .do_send(CfdAction::RejectOrder { order_id: order.id })
            .unwrap();
    }
}

/// Taker Test Setup
#[derive(Clone)]
struct Taker {
    order_feed: watch::Receiver<Option<Order>>,
    cfd_feed: watch::Receiver<Vec<Cfd>>,
    cfd_actor_addr: xtra::Address<taker_cfd::Actor<OracleActor, MonitorActor, WalletActor>>,
}

impl Taker {
    async fn start(oracle_pk: schnorrsig::PublicKey, maker_address: SocketAddr) -> Self {
        let connection::Actor {
            send_to_maker,
            read_from_maker,
        } = connection::Actor::new(maker_address)
            .await
            .expect("Taker successfully initialised");

        let db = in_memory_db().await;

        let mocks = Mocks::default();
        let (oracle, monitor, wallet) = create_actors(&mocks);

        let mut seq = Sequence::new();

        // This code should work the first time, and then fail on purpose

        mocks
            .wallet
            .lock()
            .await
            .expect_sync()
            .times(1)
            .in_sequence(&mut seq)
            .returning(|| dummy_wallet_info());
        // .times(1)
        // .returning(|| dummy_wallet_info());

        mocks
            .wallet
            .lock()
            .await
            .expect_sync()
            .times(1) // need to specify times always :(
            .in_sequence(&mut seq)
            // .returning(|| dummy_wallet_info());
            .returning(|| anyhow::bail!("there is a problem here..."));

        let wallet_addr = wallet.create(None).spawn_global();

        let taker = daemon::TakerActorSystem::new(
            db,
            wallet_addr.clone(),
            oracle_pk,
            send_to_maker,
            read_from_maker,
            |_, _| oracle,
            |_, _| async { Ok(monitor) },
        )
        .await
        .unwrap();

        wallet_addr.do_send_async(wallet::Sync {}).await.unwrap();
        wallet_addr.do_send_async(wallet::Sync {}).await.unwrap();

        Self {
            order_feed: taker.order_feed_receiver,
            cfd_feed: taker.cfd_feed_receiver,
            cfd_actor_addr: taker.cfd_actor_addr,
        }
    }

    fn take_order(&self, order: Order, quantity: Usd) {
        self.cfd_actor_addr
            .do_send(taker_cfd::TakeOffer {
                order_id: order.id,
                quantity,
            })
            .unwrap();
    }
}

async fn start_both() -> (Maker, Taker) {
    let oracle_pk: schnorrsig::PublicKey = schnorrsig::PublicKey::from_str(
        "ddd4636845a90185991826be5a494cde9f4a6947b1727217afedc6292fa4caf7",
    )
    .unwrap();

    let maker = Maker::start(oracle_pk).await;
    let taker = Taker::start(oracle_pk, maker.listen_addr).await;
    (maker, taker)
}

async fn in_memory_db() -> SqlitePool {
    // Note: Every :memory: database is distinct from every other. So, opening two database
    // connections each with the filename ":memory:" will create two independent in-memory
    // databases. see: https://www.sqlite.org/inmemorydb.html
    let pool = SqlitePool::connect(":memory:").await.unwrap();

    db::run_migrations(&pool).await.unwrap();

    pool
}
