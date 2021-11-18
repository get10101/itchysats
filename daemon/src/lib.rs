#![cfg_attr(not(test), warn(clippy::unwrap_used))]
#![warn(clippy::disallowed_method)]

use crate::db::load_all_cfds;
use crate::maker_cfd::{FromTaker, NewTakerOnline};
use crate::model::cfd::{Cfd, Order, UpdateCfdProposals};
use crate::oracle::Attestation;
use crate::tokio_ext::FutureExt;
use anyhow::Result;
use connection::ConnectionStatus;
use futures::future::RemoteHandle;
use maia::secp256k1_zkp::schnorrsig;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;
use tokio::sync::watch;
use xtra::message_channel::{MessageChannel, StrongMessageChannel};
use xtra::{Actor, Address};

pub mod actors;
pub mod auth;
pub mod bitmex_price_feed;
pub mod cfd_actors;
pub mod connection;
pub mod db;
pub mod fan_out;
pub mod forward_only_ok;
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
pub mod routes;
pub mod seed;
pub mod send_to_socket;
pub mod setup_contract;
pub mod taker_cfd;
pub mod to_sse_event;
pub mod tokio_ext;
pub mod try_continue;
pub mod wallet;
pub mod wallet_sync;
pub mod wire;

pub const HEARTBEAT_INTERVAL: std::time::Duration = Duration::from_secs(5);

pub const N_PAYOUTS: usize = 200;

/// Struct controlling the lifetime of the async tasks,
/// such as running actors and periodic notifications.
/// If it gets dropped, all tasks are cancelled.
pub struct Tasks(Vec<RemoteHandle<()>>);

pub struct MakerActorSystem<O, M, T, W> {
    pub cfd_actor_addr: Address<maker_cfd::Actor<O, M, T, W>>,
    pub cfd_feed_receiver: watch::Receiver<Vec<Cfd>>,
    pub order_feed_receiver: watch::Receiver<Option<Order>>,
    pub update_cfd_feed_receiver: watch::Receiver<UpdateCfdProposals>,
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
        + xtra::Handler<maker_inc_connections::BroadcastOrder>,
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
            Box<dyn MessageChannel<NewTakerOnline>>,
            Box<dyn MessageChannel<FromTaker>>,
        ) -> T,
        settlement_time_interval_hours: time::Duration,
        n_payouts: usize,
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

        let mut tasks = vec![];

        let (cfd_actor_addr, cfd_actor_fut) = maker_cfd::Actor::new(
            db,
            wallet_addr,
            settlement_time_interval_hours,
            oracle_pk,
            cfd_feed_sender,
            order_feed_sender,
            update_cfd_feed_sender,
            inc_conn_addr.clone(),
            monitor_addr.clone(),
            oracle_addr.clone(),
            n_payouts,
        )
        .create(None)
        .run();

        tasks.push(cfd_actor_fut.spawn_with_handle());

        tasks.push(
            inc_conn_ctx
                .run(inc_conn_constructor(
                    Box::new(cfd_actor_addr.clone()),
                    Box::new(cfd_actor_addr.clone()),
                ))
                .spawn_with_handle(),
        );

        tasks.push(
            monitor_ctx
                .notify_interval(Duration::from_secs(20), || monitor::Sync)
                .map_err(|e| anyhow::anyhow!(e))?
                .spawn_with_handle(),
        );
        tasks.push(
            monitor_ctx
                .run(monitor_constructor(Box::new(cfd_actor_addr.clone()), cfds.clone()).await?)
                .spawn_with_handle(),
        );

        tasks.push(
            oracle_ctx
                .notify_interval(Duration::from_secs(5), || oracle::Sync)
                .map_err(|e| anyhow::anyhow!(e))?
                .spawn_with_handle(),
        );
        let (fan_out_actor, fan_out_actor_fut) =
            fan_out::Actor::new(&[&cfd_actor_addr, &monitor_addr])
                .create(None)
                .run();
        tasks.push(fan_out_actor_fut.spawn_with_handle());

        tasks.push(
            oracle_ctx
                .run(oracle_constructor(cfds, Box::new(fan_out_actor)))
                .spawn_with_handle(),
        );

        oracle_addr.send(oracle::Sync).await?;

        tracing::debug!("Maker actor system ready");

        Ok(Self {
            cfd_actor_addr,
            cfd_feed_receiver,
            order_feed_receiver,
            update_cfd_feed_receiver,
            inc_conn_addr,
            tasks: Tasks(tasks),
        })
    }
}

pub struct TakerActorSystem<O, M, W> {
    pub cfd_actor_addr: Address<taker_cfd::Actor<O, M, W>>,
    pub connection_actor_addr: Address<connection::Actor>,
    pub cfd_feed_receiver: watch::Receiver<Vec<Cfd>>,
    pub order_feed_receiver: watch::Receiver<Option<Order>>,
    pub update_cfd_feed_receiver: watch::Receiver<UpdateCfdProposals>,
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

        let (maker_online_status_feed_sender, maker_online_status_feed_receiver) =
            watch::channel(ConnectionStatus::Offline);

        let (monitor_addr, mut monitor_ctx) = xtra::Context::new(None);
        let (oracle_addr, mut oracle_ctx) = xtra::Context::new(None);

        let mut tasks = vec![];

        let (connection_actor_addr, connection_actor_ctx) = xtra::Context::new(None);
        let (cfd_actor_addr, cfd_actor_fut) = taker_cfd::Actor::new(
            db,
            wallet_addr,
            oracle_pk,
            cfd_feed_sender,
            order_feed_sender,
            update_cfd_feed_sender,
            Box::new(connection_actor_addr.clone()),
            monitor_addr.clone(),
            oracle_addr,
            n_payouts,
        )
        .create(None)
        .run();

        tasks.push(cfd_actor_fut.spawn_with_handle());

        tasks.push(
            connection_actor_ctx
                .run(connection::Actor::new(
                    maker_online_status_feed_sender,
                    Box::new(cfd_actor_addr.clone()),
                    identity_sk,
                    maker_heartbeat_interval,
                ))
                .spawn_with_handle(),
        );

        tasks.push(
            monitor_ctx
                .notify_interval(Duration::from_secs(20), || monitor::Sync)
                .map_err(|e| anyhow::anyhow!(e))?
                .spawn_with_handle(),
        );
        tasks.push(
            monitor_ctx
                .run(monitor_constructor(Box::new(cfd_actor_addr.clone()), cfds.clone()).await?)
                .spawn_with_handle(),
        );

        tasks.push(
            oracle_ctx
                .notify_interval(Duration::from_secs(5), || oracle::Sync)
                .map_err(|e| anyhow::anyhow!(e))?
                .spawn_with_handle(),
        );

        let (fan_out_actor, fan_out_actor_fut) =
            fan_out::Actor::new(&[&cfd_actor_addr, &monitor_addr])
                .create(None)
                .run();

        tasks.push(fan_out_actor_fut.spawn_with_handle());

        tasks.push(
            oracle_ctx
                .run(oracle_constructor(cfds, Box::new(fan_out_actor)))
                .spawn_with_handle(),
        );

        tracing::debug!("Taker actor system ready");

        Ok(Self {
            cfd_actor_addr,
            connection_actor_addr,
            cfd_feed_receiver,
            order_feed_receiver,
            update_cfd_feed_receiver,
            maker_online_status_feed_receiver,
            tasks: Tasks(tasks),
        })
    }
}
