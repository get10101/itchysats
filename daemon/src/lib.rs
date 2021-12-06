#![cfg_attr(not(test), warn(clippy::unwrap_used))]
#![warn(clippy::disallowed_method)]
use crate::db::load_all_cfds;
use crate::maker_cfd::{FromTaker, TakerConnected};
use crate::model::cfd::{Cfd, Order, UpdateCfdProposals};
use crate::oracle::Attestation;
use crate::tokio_ext::FutureExt;
use address_map::Stopping;
use anyhow::Result;
use connection::ConnectionStatus;
use futures::future::RemoteHandle;
use maia::secp256k1_zkp::schnorrsig;
use maker_cfd::TakerDisconnected;
use sqlx::SqlitePool;
use std::future::Future;
use std::time::Duration;
use tokio::sync::watch;
use xtra::message_channel::{MessageChannel, StrongMessageChannel};
use xtra::{Actor, Address};

pub mod sqlx_ext; // Must come first because it is a macro.

pub mod actors;
pub mod address_map;
pub mod auth;
pub mod bitmex_price_feed;
pub mod cfd_actors;
pub mod collab_settlement_taker;
pub mod connection;
pub mod db;
pub mod fan_out;
pub mod housekeeping;
pub mod keypair;
pub mod logger;
pub mod maker_cfd;
pub mod maker_inc_connections;
pub mod model;
pub mod monitor;
mod noise;
pub mod olivia;
pub mod oracle;
pub mod payout_curve;
pub mod projection;
pub mod routes;
pub mod seed;
pub mod send_to_socket;
pub mod setup_contract;
pub mod setup_maker;
pub mod setup_taker;
pub mod taker_cfd;
pub mod to_sse_event;
pub mod tokio_ext;
pub mod try_continue;
pub mod tx;
pub mod wallet;
pub mod wallet_sync;
pub mod wire;

// Certain operations (e.g. contract setup) take long time in debug mode,
// causing us to lag behind in processing heartbeats.
// Increasing the value for debug mode makes sure that we don't cause problems
// when testing / CI, whilst the release can still detect status faster
pub const HEARTBEAT_INTERVAL: std::time::Duration =
    Duration::from_secs(if cfg!(debug_assertions) { 45 } else { 5 });

pub const N_PAYOUTS: usize = 200;

/// The interval until the cfd gets settled, i.e. the attestation happens
///
/// This variable defines at what point in time the oracle event id will be chose to settle the cfd.
/// Hence, this constant defines how long a cfd is open (until it gets either settled or rolled
/// over).
///
/// Multiple code parts align on this constant:
/// - How the oracle event id is chosen when creating an order (maker)
/// - The sliding window of cached oracle announcements (maker, taker)
/// - The auto-rollover time-window (taker)
pub const SETTLEMENT_INTERVAL: time::Duration = time::Duration::days(7);

/// Struct controlling the lifetime of the async tasks,
/// such as running actors and periodic notifications.
/// If it gets dropped, all tasks are cancelled.
#[derive(Default)]
pub struct Tasks(Vec<RemoteHandle<()>>);

impl Tasks {
    /// Spawn the task on the runtime and remembers the handle
    /// NOTE: Do *not* call spawn_with_handle() before calling `add`,
    /// such calls  will trigger panic in debug mode.
    pub fn add(&mut self, f: impl Future<Output = ()> + Send + 'static) {
        let handle = f.spawn_with_handle();
        self.0.push(handle);
    }
}

pub struct MakerActorSystem<O, M, T, W> {
    pub cfd_actor_addr: Address<maker_cfd::Actor<O, M, T, W>>,
    pub inc_conn_addr: Address<T>,
    pub tasks: Tasks,
}

impl<O, M, T, W> MakerActorSystem<O, M, T, W>
where
    O: xtra::Handler<oracle::MonitorAttestation>
        + xtra::Handler<oracle::GetAnnouncement>
        + xtra::Handler<oracle::Sync>,
    M: xtra::Handler<monitor::StartMonitoring>
        + xtra::Handler<monitor::Sync>
        + xtra::Handler<monitor::CollaborativeSettlement>
        + xtra::Handler<oracle::Attestation>,
    T: xtra::Handler<maker_inc_connections::TakerMessage>
        + xtra::Handler<maker_inc_connections::BroadcastOrder>
        + xtra::Handler<maker_inc_connections::ConfirmOrder>
        + xtra::Handler<Stopping<setup_maker::Actor>>,
    W: xtra::Handler<wallet::BuildPartyParams>
        + xtra::Handler<wallet::Sync>
        + xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::TryBroadcastTransaction>,
{
    #[allow(clippy::too_many_arguments)]
    pub async fn new<F>(
        db: SqlitePool,
        wallet_addr: Address<W>,
        oracle_pk: schnorrsig::PublicKey,
        oracle_constructor: impl FnOnce(Vec<Cfd>, Box<dyn StrongMessageChannel<Attestation>>) -> O,
        monitor_constructor: impl FnOnce(Box<dyn StrongMessageChannel<monitor::Event>>, Vec<Cfd>) -> F,
        inc_conn_constructor: impl FnOnce(
            Box<dyn MessageChannel<TakerConnected>>,
            Box<dyn MessageChannel<TakerDisconnected>>,
            Box<dyn MessageChannel<FromTaker>>,
        ) -> T,
        settlement_interval: time::Duration,
        n_payouts: usize,
        projection_actor: Address<projection::Actor>,
    ) -> Result<Self>
    where
        F: Future<Output = Result<M>>,
    {
        let mut conn = db.acquire().await?;

        let cfds = load_all_cfds(&mut conn).await?;

        let (monitor_addr, mut monitor_ctx) = xtra::Context::new(None);
        let (oracle_addr, mut oracle_ctx) = xtra::Context::new(None);
        let (inc_conn_addr, inc_conn_ctx) = xtra::Context::new(None);

        let mut tasks = Tasks::default();

        let (cfd_actor_addr, cfd_actor_fut) = maker_cfd::Actor::new(
            db,
            wallet_addr,
            settlement_interval,
            oracle_pk,
            projection_actor,
            inc_conn_addr.clone(),
            monitor_addr.clone(),
            oracle_addr.clone(),
            n_payouts,
        )
        .create(None)
        .run();

        tasks.add(cfd_actor_fut);

        tasks.add(inc_conn_ctx.run(inc_conn_constructor(
            Box::new(cfd_actor_addr.clone()),
            Box::new(cfd_actor_addr.clone()),
            Box::new(cfd_actor_addr.clone()),
        )));

        tasks.add(
            monitor_ctx
                .notify_interval(Duration::from_secs(20), || monitor::Sync)
                .map_err(|e| anyhow::anyhow!(e))?,
        );
        tasks.add(
            monitor_ctx
                .run(monitor_constructor(Box::new(cfd_actor_addr.clone()), cfds.clone()).await?),
        );

        tasks.add(
            oracle_ctx
                .notify_interval(Duration::from_secs(5), || oracle::Sync)
                .map_err(|e| anyhow::anyhow!(e))?,
        );
        let (fan_out_actor, fan_out_actor_fut) =
            fan_out::Actor::new(&[&cfd_actor_addr, &monitor_addr])
                .create(None)
                .run();
        tasks.add(fan_out_actor_fut);

        tasks.add(oracle_ctx.run(oracle_constructor(cfds, Box::new(fan_out_actor))));

        oracle_addr.send(oracle::Sync).await?;

        tracing::debug!("Maker actor system ready");

        Ok(Self {
            cfd_actor_addr,
            inc_conn_addr,
            tasks,
        })
    }
}

pub struct TakerActorSystem<O, M, W> {
    pub cfd_actor_addr: Address<taker_cfd::Actor<O, M, W>>,
    pub connection_actor_addr: Address<connection::Actor>,
    pub maker_online_status_feed_receiver: watch::Receiver<ConnectionStatus>,
    pub tasks: Tasks,
}

impl<O, M, W> TakerActorSystem<O, M, W>
where
    O: xtra::Handler<oracle::MonitorAttestation>
        + xtra::Handler<oracle::GetAnnouncement>
        + xtra::Handler<oracle::Sync>,
    M: xtra::Handler<monitor::StartMonitoring>
        + xtra::Handler<monitor::Sync>
        + xtra::Handler<monitor::CollaborativeSettlement>
        + xtra::Handler<oracle::Attestation>,
    W: xtra::Handler<wallet::BuildPartyParams>
        + xtra::Handler<wallet::Sync>
        + xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::TryBroadcastTransaction>,
{
    #[allow(clippy::too_many_arguments)]
    pub async fn new<F>(
        db: SqlitePool,
        wallet_addr: Address<W>,
        oracle_pk: schnorrsig::PublicKey,
        identity_sk: x25519_dalek::StaticSecret,
        oracle_constructor: impl FnOnce(Vec<Cfd>, Box<dyn StrongMessageChannel<Attestation>>) -> O,
        monitor_constructor: impl FnOnce(Box<dyn StrongMessageChannel<monitor::Event>>, Vec<Cfd>) -> F,
        n_payouts: usize,
        maker_heartbeat_interval: Duration,
        connect_timeout: Duration,
        projection_actor: Address<projection::Actor>,
    ) -> Result<Self>
    where
        F: Future<Output = Result<M>>,
    {
        let mut conn = db.acquire().await?;

        let cfds = load_all_cfds(&mut conn).await?;

        let (maker_online_status_feed_sender, maker_online_status_feed_receiver) =
            watch::channel(ConnectionStatus::Offline { reason: None });

        let (monitor_addr, mut monitor_ctx) = xtra::Context::new(None);
        let (oracle_addr, mut oracle_ctx) = xtra::Context::new(None);

        let mut tasks = Tasks::default();

        let (connection_actor_addr, connection_actor_ctx) = xtra::Context::new(None);
        let (cfd_actor_addr, cfd_actor_fut) = taker_cfd::Actor::new(
            db,
            wallet_addr,
            oracle_pk,
            projection_actor,
            connection_actor_addr.clone(),
            monitor_addr.clone(),
            oracle_addr,
            n_payouts,
        )
        .create(None)
        .run();

        tasks.add(cfd_actor_fut);

        tasks.add(connection_actor_ctx.run(connection::Actor::new(
            maker_online_status_feed_sender,
            Box::new(cfd_actor_addr.clone()),
            identity_sk,
            maker_heartbeat_interval,
            connect_timeout,
        )));

        tasks.add(
            monitor_ctx
                .notify_interval(Duration::from_secs(20), || monitor::Sync)
                .map_err(|e| anyhow::anyhow!(e))?,
        );
        tasks.add(
            monitor_ctx
                .run(monitor_constructor(Box::new(cfd_actor_addr.clone()), cfds.clone()).await?),
        );

        tasks.add(
            oracle_ctx
                .notify_interval(Duration::from_secs(5), || oracle::Sync)
                .map_err(|e| anyhow::anyhow!(e))?,
        );

        let (fan_out_actor, fan_out_actor_fut) =
            fan_out::Actor::new(&[&cfd_actor_addr, &monitor_addr])
                .create(None)
                .run();

        tasks.add(fan_out_actor_fut);

        tasks.add(oracle_ctx.run(oracle_constructor(cfds, Box::new(fan_out_actor))));

        tracing::debug!("Taker actor system ready");

        Ok(Self {
            cfd_actor_addr,
            connection_actor_addr,
            maker_online_status_feed_receiver,
            tasks,
        })
    }
}
