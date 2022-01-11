#![cfg_attr(not(test), warn(clippy::unwrap_used))]
#![warn(clippy::disallowed_method)]
use crate::bitcoin::Txid;
use crate::model::cfd::Order;
use crate::model::cfd::OrderId;
use crate::model::cfd::Role;
use crate::model::Identity;
use crate::model::Price;
use crate::model::Usd;
use crate::oracle::Attestation;
use crate::tokio_ext::FutureExt;
use address_map::Stopping;
use anyhow::Result;
use bdk::bitcoin;
use bdk::bitcoin::Amount;
use bdk::FeeRate;
use connection::ConnectionStatus;
use futures::future::RemoteHandle;
use maia::secp256k1_zkp::schnorrsig;
use sqlx::SqlitePool;
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::watch;
use xtra::message_channel::StrongMessageChannel;
use xtra::Actor;
use xtra::Address;

pub mod sqlx_ext; // Must come first because it is a macro.

pub mod address_map;
pub mod auth;
pub mod auto_rollover;
pub mod bdk_ext;
pub mod bitmex_price_feed;
pub mod cfd_actors;
pub mod collab_settlement_maker;
pub mod collab_settlement_taker;
pub mod command;
pub mod connection;
pub mod db;
pub mod fan_out;
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
pub mod process_manager;
pub mod projection;
pub mod rollover_maker;
pub mod rollover_taker;
pub mod routes;
pub mod seed;
pub mod send_async_safe;
pub mod setup_contract;
pub mod setup_maker;
pub mod setup_taker;
pub mod supervisor;
pub mod taker_cfd;
pub mod to_sse_event;
pub mod tokio_ext;
pub mod try_continue;
pub mod wallet;
pub mod wire;
pub mod xtra_ext;

/// Duration between the heartbeats sent by the maker, used by the taker to
/// determine whether the maker is online.
pub const HEARTBEAT_INTERVAL: std::time::Duration = Duration::from_secs(5);

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
pub const SETTLEMENT_INTERVAL: time::Duration = time::Duration::hours(24);

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

    /// Spawn a fallible task on the runtime and remembers the handle.
    ///
    /// The task will be stopped if this instance of [`Tasks`] goes out of scope.
    /// If the task fails, the `err_handler` will be invoked.
    pub fn add_fallible<E, EF>(
        &mut self,
        f: impl Future<Output = Result<(), E>> + Send + 'static,
        err_handler: impl FnOnce(E) -> EF + Send + 'static,
    ) where
        E: Send + 'static,
        EF: Future<Output = ()> + Send + 'static,
    {
        let fut = async move {
            match f.await {
                Ok(()) => {}
                Err(err) => err_handler(err).await,
            }
        };

        let handle = fut.spawn_with_handle();
        self.0.push(handle);
    }
}

pub struct MakerActorSystem<O, W> {
    pub cfd_actor: Address<maker_cfd::Actor<O, maker_inc_connections::Actor, W>>,
    wallet_actor: Address<W>,

    _tasks: Tasks,
}

impl<O, W> MakerActorSystem<O, W>
where
    O: xtra::Handler<oracle::MonitorAttestation>
        + xtra::Handler<oracle::GetAnnouncement>
        + xtra::Handler<oracle::Sync>,
    W: xtra::Handler<wallet::BuildPartyParams>
        + xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::Withdraw>,
{
    #[allow(clippy::too_many_arguments)]
    pub async fn new<FO, FM, M>(
        db: SqlitePool,
        wallet_addr: Address<W>,
        oracle_pk: schnorrsig::PublicKey,
        oracle_constructor: impl FnOnce(Box<dyn StrongMessageChannel<Attestation>>) -> FO,
        monitor_constructor: impl FnOnce(Box<dyn StrongMessageChannel<monitor::Event>>) -> FM,
        settlement_interval: time::Duration,
        n_payouts: usize,
        projection_actor: Address<projection::Actor>,
        identity: x25519_dalek::StaticSecret,
        heartbeat_interval: Duration,
        p2p_socket: SocketAddr,
    ) -> Result<Self>
    where
        M: xtra::Handler<monitor::StartMonitoring>
            + xtra::Handler<monitor::Sync>
            + xtra::Handler<monitor::CollaborativeSettlement>
            + xtra::Handler<monitor::TryBroadcastTransaction>
            + xtra::Handler<oracle::Attestation>,
        FO: Future<Output = Result<O>>,
        FM: Future<Output = Result<M>>,
    {
        let (monitor_addr, monitor_ctx) = xtra::Context::new(None);
        let (oracle_addr, oracle_ctx) = xtra::Context::new(None);
        let (inc_conn_addr, inc_conn_ctx) = xtra::Context::new(None);
        let (process_manager_addr, process_manager_ctx) = xtra::Context::new(None);

        let mut tasks = Tasks::default();

        tasks.add(process_manager_ctx.run(process_manager::Actor::new(
            db.clone(),
            Role::Maker,
            &projection_actor,
            &monitor_addr,
            &monitor_addr,
            &monitor_addr,
            &oracle_addr,
        )));

        let (cfd_actor_addr, cfd_actor_fut) = maker_cfd::Actor::new(
            db.clone(),
            wallet_addr.clone(),
            settlement_interval,
            oracle_pk,
            projection_actor,
            process_manager_addr.clone(),
            inc_conn_addr.clone(),
            oracle_addr.clone(),
            n_payouts,
        )
        .create(None)
        .run();

        tasks.add(cfd_actor_fut);

        tasks.add(inc_conn_ctx.run(maker_inc_connections::Actor::new(
            Box::new(cfd_actor_addr.clone()),
            Box::new(cfd_actor_addr.clone()),
            Box::new(cfd_actor_addr.clone()),
            identity,
            heartbeat_interval,
            p2p_socket,
        )));

        tasks.add(monitor_ctx.run(monitor_constructor(Box::new(cfd_actor_addr.clone())).await?));

        let (fan_out_actor, fan_out_actor_fut) =
            fan_out::Actor::new(&[&cfd_actor_addr, &monitor_addr])
                .create(None)
                .run();
        tasks.add(fan_out_actor_fut);

        tasks.add(oracle_ctx.run(oracle_constructor(Box::new(fan_out_actor)).await?));

        oracle_addr.send(oracle::Sync).await?;

        tracing::debug!("Maker actor system ready");

        Ok(Self {
            cfd_actor: cfd_actor_addr,
            wallet_actor: wallet_addr,
            _tasks: tasks,
        })
    }

    pub async fn new_order(
        &self,
        price: Price,
        min_quantity: Usd,
        max_quantity: Usd,
        fee_rate: Option<u32>,
    ) -> Result<()> {
        self.cfd_actor
            .send(maker_cfd::NewOrder {
                price,
                min_quantity,
                max_quantity,
                fee_rate: fee_rate.unwrap_or(1),
            })
            .await??;

        Ok(())
    }

    pub async fn accept_order(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor
            .send(maker_cfd::AcceptOrder { order_id })
            .await??;
        Ok(())
    }

    pub async fn reject_order(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor
            .send(maker_cfd::RejectOrder { order_id })
            .await??;
        Ok(())
    }

    pub async fn accept_settlement(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor
            .send(maker_cfd::AcceptSettlement { order_id })
            .await??;
        Ok(())
    }

    pub async fn reject_settlement(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor
            .send(maker_cfd::RejectSettlement { order_id })
            .await??;
        Ok(())
    }

    pub async fn accept_rollover(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor
            .send(maker_cfd::AcceptRollover { order_id })
            .await??;
        Ok(())
    }

    pub async fn reject_rollover(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor
            .send(maker_cfd::RejectRollover { order_id })
            .await??;
        Ok(())
    }
    pub async fn commit(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor
            .send(maker_cfd::Commit { order_id })
            .await??;
        Ok(())
    }

    pub async fn withdraw(
        &self,
        amount: Option<Amount>,
        address: bitcoin::Address,
        fee: f32,
    ) -> Result<Txid> {
        self.wallet_actor
            .send(wallet::Withdraw {
                amount,
                address,
                fee: Some(bdk::FeeRate::from_sat_per_vb(fee)),
            })
            .await?
    }
}

pub struct TakerActorSystem<O, W> {
    pub cfd_actor: Address<taker_cfd::Actor<O, W>>,
    pub connection_actor: Address<connection::Actor>,
    wallet_actor: Address<W>,
    pub auto_rollover_actor: Address<auto_rollover::Actor<O>>,

    pub maker_online_status_feed_receiver: watch::Receiver<ConnectionStatus>,

    _tasks: Tasks,
}

impl<O, W> TakerActorSystem<O, W>
where
    O: xtra::Handler<oracle::MonitorAttestation>
        + xtra::Handler<oracle::GetAnnouncement>
        + xtra::Handler<oracle::Sync>,
    W: xtra::Handler<wallet::BuildPartyParams>
        + xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::Withdraw>,
{
    #[allow(clippy::too_many_arguments)]
    pub async fn new<FM, FO, M>(
        db: SqlitePool,
        wallet_actor_addr: Address<W>,
        oracle_pk: schnorrsig::PublicKey,
        identity_sk: x25519_dalek::StaticSecret,
        oracle_constructor: impl FnOnce(Box<dyn StrongMessageChannel<Attestation>>) -> FO,
        monitor_constructor: impl FnOnce(Box<dyn StrongMessageChannel<monitor::Event>>) -> FM,
        n_payouts: usize,
        maker_heartbeat_interval: Duration,
        connect_timeout: Duration,
        projection_actor: Address<projection::Actor>,
        maker_identity: Identity,
    ) -> Result<Self>
    where
        M: xtra::Handler<monitor::StartMonitoring>
            + xtra::Handler<monitor::Sync>
            + xtra::Handler<monitor::CollaborativeSettlement>
            + xtra::Handler<oracle::Attestation>
            + xtra::Handler<monitor::TryBroadcastTransaction>,
        FO: Future<Output = Result<O>>,
        FM: Future<Output = Result<M>>,
    {
        let (maker_online_status_feed_sender, maker_online_status_feed_receiver) =
            watch::channel(ConnectionStatus::Offline { reason: None });

        let (monitor_addr, monitor_ctx) = xtra::Context::new(None);
        let (oracle_addr, oracle_ctx) = xtra::Context::new(None);
        let (process_manager_addr, process_manager_ctx) = xtra::Context::new(None);

        let mut tasks = Tasks::default();

        tasks.add(process_manager_ctx.run(process_manager::Actor::new(
            db.clone(),
            Role::Taker,
            &projection_actor,
            &monitor_addr,
            &monitor_addr,
            &monitor_addr,
            &oracle_addr,
        )));

        let (connection_actor_addr, connection_actor_ctx) = xtra::Context::new(None);
        let (cfd_actor_addr, cfd_actor_fut) = taker_cfd::Actor::new(
            db.clone(),
            wallet_actor_addr.clone(),
            oracle_pk,
            projection_actor.clone(),
            process_manager_addr.clone(),
            connection_actor_addr.clone(),
            oracle_addr.clone(),
            n_payouts,
            maker_identity,
        )
        .create(None)
        .run();

        let (auto_rollover_addr, auto_rollover_fut) = auto_rollover::Actor::new(
            db,
            oracle_pk,
            process_manager_addr,
            connection_actor_addr.clone(),
            oracle_addr,
            n_payouts,
        )
        .create(None)
        .run();

        tasks.add(cfd_actor_fut);
        tasks.add(auto_rollover_fut);

        // Timeout happens when taker did not receive two consecutive heartbeats
        let taker_heartbeat_timeout = maker_heartbeat_interval
            .checked_mul(2)
            .expect("not to overflow");

        tasks.add(connection_actor_ctx.run(connection::Actor::new(
            maker_online_status_feed_sender,
            &cfd_actor_addr,
            identity_sk,
            taker_heartbeat_timeout,
            connect_timeout,
        )));

        tasks.add(monitor_ctx.run(monitor_constructor(Box::new(cfd_actor_addr.clone())).await?));

        let (fan_out_actor, fan_out_actor_fut) =
            fan_out::Actor::new(&[&cfd_actor_addr, &monitor_addr])
                .create(None)
                .run();

        tasks.add(fan_out_actor_fut);

        tasks.add(oracle_ctx.run(oracle_constructor(Box::new(fan_out_actor)).await?));

        tracing::debug!("Taker actor system ready");

        Ok(Self {
            cfd_actor: cfd_actor_addr,
            connection_actor: connection_actor_addr,
            wallet_actor: wallet_actor_addr,
            auto_rollover_actor: auto_rollover_addr,
            maker_online_status_feed_receiver,
            _tasks: tasks,
        })
    }

    pub async fn take_offer(&self, order_id: OrderId, quantity: Usd) -> Result<()> {
        self.cfd_actor
            .send(taker_cfd::TakeOffer { order_id, quantity })
            .await??;
        Ok(())
    }

    pub async fn commit(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor.send(taker_cfd::Commit { order_id }).await?
    }

    pub async fn propose_settlement(&self, order_id: OrderId, current_price: Price) -> Result<()> {
        self.cfd_actor
            .send(taker_cfd::ProposeSettlement {
                order_id,
                current_price,
            })
            .await?
    }

    pub async fn withdraw(
        &self,
        amount: Option<Amount>,
        address: bitcoin::Address,
        fee_rate: FeeRate,
    ) -> Result<Txid> {
        self.wallet_actor
            .send(wallet::Withdraw {
                amount,
                address,
                fee: Some(fee_rate),
            })
            .await?
    }
}
