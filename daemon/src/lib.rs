#![cfg_attr(not(test), warn(clippy::unwrap_used))]
#![warn(clippy::disallowed_method)]
use crate::bitcoin::Txid;
use crate::maker_cfd::FromTaker;
use crate::maker_cfd::TakerConnected;
use crate::model::cfd::Cfd;
use crate::model::cfd::Order;
use crate::model::cfd::OrderId;
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
use maker_cfd::TakerDisconnected;
use sqlx::SqlitePool;
use std::future::Future;
use std::task::Poll;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use xtra::message_channel::MessageChannel;
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
}

pub struct MakerActorSystem<O, M, T, W> {
    pub cfd_actor_addr: Address<maker_cfd::Actor<O, M, T, W>>,
    wallet_actor_addr: Address<W>,
    inc_conn_addr: Address<T>,
    _tasks: Tasks,
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
        + xtra::Handler<Stopping<setup_maker::Actor>>
        + xtra::Handler<maker_inc_connections::settlement::Response>
        + xtra::Handler<Stopping<collab_settlement_maker::Actor>>
        + xtra::Handler<Stopping<rollover_maker::Actor>>
        + xtra::Handler<maker_cfd::RollOverProposed>
        + xtra::Handler<maker_inc_connections::ListenerMessage>,
    W: xtra::Handler<wallet::BuildPartyParams>
        + xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::TryBroadcastTransaction>
        + xtra::Handler<wallet::Withdraw>,
{
    #[allow(clippy::too_many_arguments)]
    pub async fn new<FO, FM>(
        db: SqlitePool,
        wallet_addr: Address<W>,
        oracle_pk: schnorrsig::PublicKey,
        oracle_constructor: impl FnOnce(Box<dyn StrongMessageChannel<Attestation>>) -> FO,
        monitor_constructor: impl FnOnce(Box<dyn StrongMessageChannel<monitor::Event>>) -> FM,
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
        FO: Future<Output = Result<O>>,
        FM: Future<Output = Result<M>>,
    {
        let (monitor_addr, monitor_ctx) = xtra::Context::new(None);
        let (oracle_addr, oracle_ctx) = xtra::Context::new(None);
        let (inc_conn_addr, inc_conn_ctx) = xtra::Context::new(None);

        let mut tasks = Tasks::default();

        let (cfd_actor_addr, cfd_actor_fut) = maker_cfd::Actor::new(
            db,
            wallet_addr.clone(),
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
            cfd_actor_addr,
            wallet_actor_addr: wallet_addr,
            inc_conn_addr,
            _tasks: tasks,
        })
    }

    pub fn listen_on(&mut self, listener: TcpListener) {
        let listener_stream = futures::stream::poll_fn(move |ctx| {
            let message = match futures::ready!(listener.poll_accept(ctx)) {
                Ok((stream, address)) => {
                    maker_inc_connections::ListenerMessage::NewConnection { stream, address }
                }
                Err(e) => maker_inc_connections::ListenerMessage::Error { source: e },
            };

            Poll::Ready(Some(message))
        });

        self._tasks
            .add(self.inc_conn_addr.clone().attach_stream(listener_stream));
    }

    pub async fn new_order(
        &self,
        price: Price,
        min_quantity: Usd,
        max_quantity: Usd,
        fee_rate: Option<u32>,
    ) -> Result<()> {
        self.cfd_actor_addr
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
        self.cfd_actor_addr
            .send(maker_cfd::AcceptOrder { order_id })
            .await??;
        Ok(())
    }

    pub async fn reject_order(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor_addr
            .send(maker_cfd::RejectOrder { order_id })
            .await??;
        Ok(())
    }

    pub async fn accept_settlement(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor_addr
            .send(maker_cfd::AcceptSettlement { order_id })
            .await??;
        Ok(())
    }

    pub async fn reject_settlement(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor_addr
            .send(maker_cfd::RejectSettlement { order_id })
            .await??;
        Ok(())
    }

    pub async fn accept_rollover(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor_addr
            .send(maker_cfd::AcceptRollOver { order_id })
            .await??;
        Ok(())
    }

    pub async fn reject_rollover(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor_addr
            .send(maker_cfd::RejectRollOver { order_id })
            .await??;
        Ok(())
    }
    pub async fn commit(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor_addr
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
        self.wallet_actor_addr
            .send(wallet::Withdraw {
                amount,
                address,
                fee: Some(bdk::FeeRate::from_sat_per_vb(fee)),
            })
            .await?
    }
}

pub struct TakerActorSystem<O, M, W> {
    pub cfd_actor_addr: Address<taker_cfd::Actor<O, M, W>>,
    pub connection_actor_addr: Address<connection::Actor>,
    pub maker_online_status_feed_receiver: watch::Receiver<ConnectionStatus>,
    wallet_actor_addr: Address<W>,
    _tasks: Tasks,
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
        + xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::TryBroadcastTransaction>
        + xtra::Handler<wallet::Withdraw>
        + xtra::Handler<wallet::Reinitialise>,
{
    #[allow(clippy::too_many_arguments)]
    pub async fn new<FM, FO>(
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
        FO: Future<Output = Result<O>>,
        FM: Future<Output = Result<M>>,
    {
        let (maker_online_status_feed_sender, maker_online_status_feed_receiver) =
            watch::channel(ConnectionStatus::Offline { reason: None });

        let (monitor_addr, monitor_ctx) = xtra::Context::new(None);
        let (oracle_addr, oracle_ctx) = xtra::Context::new(None);

        let mut tasks = Tasks::default();

        let (connection_actor_addr, connection_actor_ctx) = xtra::Context::new(None);
        let (cfd_actor_addr, cfd_actor_fut) = taker_cfd::Actor::new(
            db.clone(),
            wallet_actor_addr.clone(),
            oracle_pk,
            projection_actor.clone(),
            connection_actor_addr.clone(),
            monitor_addr.clone(),
            oracle_addr.clone(),
            n_payouts,
            maker_identity,
        )
        .create(None)
        .run();

        let (auto_rollover_address, auto_rollover_fut) = auto_rollover::Actor::new(
            db,
            oracle_pk,
            projection_actor,
            connection_actor_addr.clone(),
            monitor_addr.clone(),
            oracle_addr,
            n_payouts,
        )
        .create(None)
        .run();
        std::mem::forget(auto_rollover_address); // leak this address to avoid shutdown

        tasks.add(cfd_actor_fut);
        tasks.add(auto_rollover_fut);

        tasks.add(connection_actor_ctx.run(connection::Actor::new(
            maker_online_status_feed_sender,
            &cfd_actor_addr,
            identity_sk,
            maker_heartbeat_interval,
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
            cfd_actor_addr,
            connection_actor_addr,
            maker_online_status_feed_receiver,
            wallet_actor_addr,
            _tasks: tasks,
        })
    }

    pub async fn take_offer(&self, order_id: OrderId, quantity: Usd) -> Result<()> {
        self.cfd_actor_addr
            .send(taker_cfd::TakeOffer { order_id, quantity })
            .await??;
        Ok(())
    }

    pub async fn commit(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor_addr
            .send(taker_cfd::Commit { order_id })
            .await?
    }

    pub async fn propose_settlement(&self, order_id: OrderId, current_price: Price) -> Result<()> {
        self.cfd_actor_addr
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
        self.wallet_actor_addr
            .send(wallet::Withdraw {
                amount,
                address,
                fee: Some(fee_rate),
            })
            .await?
    }

    pub async fn reinitialise_wallet(&self, seed_words: &str) -> Result<()> {
        self.wallet_actor_addr
            .send(wallet::Reinitialise {
                seed_words: seed_words.to_string(),
            })
            .await?
    }
}
