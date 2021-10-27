use anyhow::Result;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::{ecdsa, Txid};
use cfd_protocol::secp256k1_zkp::{schnorrsig, Secp256k1};
use cfd_protocol::PartyParams;
use daemon::model::cfd::Order;
use daemon::model::{Price, Usd, WalletInfo};
use daemon::{connection, db, logger, maker_cfd, maker_inc_connections, monitor, oracle, wallet};
use rand::thread_rng;
use rust_decimal_macros::dec;
use sqlx::SqlitePool;
use std::net::SocketAddr;
use std::str::FromStr;
use std::task::Poll;
use std::time::SystemTime;
use tokio::sync::watch;
use tracing_subscriber::filter::LevelFilter;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor;
use xtra_productivity::xtra_productivity;

#[tokio::test]
async fn taker_receives_order_from_maker_on_publication() {
    let (mut maker, mut taker) = start_both().await;

    assert!(is_next_none(&mut taker.order_feed).await);

    let (published, received) = tokio::join!(
        maker.publish_order(new_dummy_order()),
        next_some(&mut taker.order_feed)
    );

    // TODO: Add assertion function so we can assert on the other order values
    assert_eq!(published.id, received.id);
}

fn new_dummy_order() -> maker_cfd::NewOrder {
    maker_cfd::NewOrder {
        price: Price::new(dec!(50_000)).expect("unexpected failure"),
        min_quantity: Usd::new(dec!(10)),
        max_quantity: Usd::new(dec!(100)),
    }
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
    rx.changed().await.unwrap();
    rx.borrow().clone()
}

fn init_tracing() {
    logger::init(LevelFilter::DEBUG, false).unwrap();
    tracing::info!("Running version: {}", env!("VERGEN_GIT_SEMVER_LIGHTWEIGHT"));
}

/// Test Stub simulating the Oracle actor
struct Oracle;
impl xtra::Actor for Oracle {}

#[xtra_productivity(message_impl = false)]
impl Oracle {
    async fn handle_get_announcement(
        &mut self,
        _msg: oracle::GetAnnouncement,
    ) -> Option<oracle::Announcement> {
        todo!("stub this if needed")
    }

    async fn handle(&mut self, _msg: oracle::MonitorAttestation) {
        todo!("stub this if needed")
    }

    async fn handle(&mut self, _msg: oracle::Sync) {}
}

/// Test Stub simulating the Monitor actor
struct Monitor;
impl xtra::Actor for Monitor {}

#[xtra_productivity(message_impl = false)]
impl Monitor {
    async fn handle(&mut self, _msg: monitor::Sync) {}

    async fn handle(&mut self, _msg: monitor::StartMonitoring) {
        todo!("stub this if needed")
    }

    async fn handle(&mut self, _msg: monitor::CollaborativeSettlement) {
        todo!("stub this if needed")
    }

    async fn handle(&mut self, _msg: oracle::Attestation) {
        todo!("stub this if needed")
    }
}

/// Test Stub simulating the Wallet actor
struct Wallet;
impl xtra::Actor for Wallet {}

#[xtra_productivity(message_impl = false)]
impl Wallet {
    async fn handle(&mut self, _msg: wallet::BuildPartyParams) -> Result<PartyParams> {
        todo!("stub this if needed")
    }
    async fn handle(&mut self, _msg: wallet::Sync) -> Result<WalletInfo> {
        let s = Secp256k1::new();
        let public_key = ecdsa::PublicKey::new(s.generate_keypair(&mut thread_rng()).1);
        let address = bdk::bitcoin::Address::p2pkh(&public_key, bdk::bitcoin::Network::Testnet);

        Ok(WalletInfo {
            balance: bdk::bitcoin::Amount::ONE_BTC,
            address,
            last_updated_at: SystemTime::now(),
        })
    }
    async fn handle(&mut self, _msg: wallet::Sign) -> Result<PartiallySignedTransaction> {
        todo!("stub this if needed")
    }
    async fn handle(&mut self, _msg: wallet::TryBroadcastTransaction) -> Result<Txid> {
        todo!("stub this if needed")
    }
}

/// Maker Test Setup
struct Maker {
    cfd_actor_addr:
        xtra::Address<maker_cfd::Actor<Oracle, Monitor, maker_inc_connections::Actor, Wallet>>,
    order_feed_receiver: watch::Receiver<Option<Order>>,
    #[allow(dead_code)] // we need to keep the xtra::Address for refcounting
    inc_conn_addr: xtra::Address<maker_inc_connections::Actor>,
    address: SocketAddr,
}

impl Maker {
    async fn start(oracle_pk: schnorrsig::PublicKey) -> Self {
        let db = in_memory_db().await;

        let wallet_addr = Wallet {}.create(None).spawn_global();

        let settlement_time_interval_hours = time::Duration::hours(24);

        let maker = daemon::MakerActorSystem::new(
            db,
            wallet_addr,
            oracle_pk,
            |_, _| Oracle,
            |_, _| async { Ok(Monitor) },
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
            order_feed_receiver: maker.order_feed_receiver,
            inc_conn_addr: maker.inc_conn_addr,
            address,
        }
    }

    async fn publish_order(&mut self, new_order_params: maker_cfd::NewOrder) -> Order {
        self.cfd_actor_addr.send(new_order_params).await.unwrap();
        let next_order = self.order_feed_receiver.borrow().clone().unwrap();

        next_order
    }
}

/// Taker Test Setup
struct Taker {
    order_feed: watch::Receiver<Option<Order>>,
}

impl Taker {
    async fn start(oracle_pk: schnorrsig::PublicKey, maker_address: SocketAddr) -> Self {
        let connection::Actor {
            send_to_maker,
            read_from_maker,
        } = connection::Actor::new(maker_address).await;

        let db = in_memory_db().await;

        let wallet_addr = Wallet {}.create(None).spawn_global();

        let taker = daemon::TakerActorSystem::new(
            db,
            wallet_addr,
            oracle_pk,
            send_to_maker,
            read_from_maker,
            |_, _| Oracle,
            |_, _| async { Ok(Monitor) },
        )
        .await
        .unwrap();

        Self {
            order_feed: taker.order_feed_receiver,
        }
    }
}

async fn start_both() -> (Maker, Taker) {
    init_tracing();
    let oracle_pk: schnorrsig::PublicKey = schnorrsig::PublicKey::from_str(
        "ddd4636845a90185991826be5a494cde9f4a6947b1727217afedc6292fa4caf7",
    )
    .unwrap();

    let maker = Maker::start(oracle_pk).await;
    let taker = Taker::start(oracle_pk, maker.address).await;
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
