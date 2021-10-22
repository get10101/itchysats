use crate::db::load_all_cfds;
use crate::maker_cfd::{FromTaker, NewTakerOnline};
use crate::model::cfd::{Cfd, Order, UpdateCfdProposals};
use crate::oracle::Attestation;
use crate::wallet::Wallet;
use anyhow::Result;
use cfd_protocol::secp256k1_zkp::schnorrsig;
use futures::Stream;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;
use tokio::sync::watch;
use xtra::message_channel::{MessageChannel, StrongMessageChannel};
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::{Actor, Address};

pub mod actors;
pub mod auth;
pub mod bitmex_price_feed;
pub mod cfd_actors;
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

pub struct Maker<O, M, T> {
    pub cfd_actor_addr: Address<maker_cfd::Actor<O, M, T>>,
    pub cfd_feed_receiver: watch::Receiver<Vec<Cfd>>,
    pub order_feed_receiver: watch::Receiver<Option<Order>>,
    pub update_cfd_feed_receiver: watch::Receiver<UpdateCfdProposals>,
    pub inc_conn_addr: Address<T>,
}

impl<O, M, T> Maker<O, M, T>
where
    O: xtra::Handler<oracle::MonitorAttestation>
        + xtra::Handler<oracle::GetAnnouncement>
        + xtra::Handler<oracle::FetchAnnouncement>
        + xtra::Handler<oracle::Sync>,
    M: xtra::Handler<monitor::StartMonitoring>
        + xtra::Handler<monitor::Sync>
        + xtra::Handler<monitor::CollaborativeSettlement>
        + xtra::Handler<oracle::Attestation>,
    T: xtra::Handler<maker_inc_connections::TakerMessage>
        + xtra::Handler<maker_inc_connections::BroadcastOrder>,
{
    pub async fn new<F>(
        db: SqlitePool,
        wallet: Wallet,
        oracle_pk: schnorrsig::PublicKey,
        oracle_constructor: impl Fn(Vec<Cfd>, Box<dyn StrongMessageChannel<Attestation>>) -> O,
        monitor_constructor: impl Fn(Box<dyn StrongMessageChannel<monitor::Event>>, Vec<Cfd>) -> F,
        inc_conn_constructor: impl Fn(
            Box<dyn MessageChannel<NewTakerOnline>>,
            Box<dyn MessageChannel<FromTaker>>,
        ) -> T,
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

        let cfd_actor_addr = maker_cfd::Actor::new(
            db,
            wallet,
            oracle_pk,
            cfd_feed_sender,
            order_feed_sender,
            update_cfd_feed_sender,
            inc_conn_addr.clone(),
            monitor_addr.clone(),
            oracle_addr.clone(),
        )
        .create(None)
        .spawn_global();

        tokio::spawn(inc_conn_ctx.run(inc_conn_constructor(
            Box::new(cfd_actor_addr.clone()),
            Box::new(cfd_actor_addr.clone()),
        )));

        tokio::spawn(
            monitor_ctx
                .notify_interval(Duration::from_secs(20), || monitor::Sync)
                .map_err(|e| anyhow::anyhow!(e))?,
        );
        tokio::spawn(
            monitor_ctx
                .run(monitor_constructor(Box::new(cfd_actor_addr.clone()), cfds.clone()).await?),
        );

        tokio::spawn(
            oracle_ctx
                .notify_interval(Duration::from_secs(5), || oracle::Sync)
                .map_err(|e| anyhow::anyhow!(e))?,
        );
        let fan_out_actor = fan_out::Actor::new(&[&cfd_actor_addr, &monitor_addr])
            .create(None)
            .spawn_global();

        tokio::spawn(oracle_ctx.run(oracle_constructor(cfds, Box::new(fan_out_actor))));

        oracle_addr.do_send_async(oracle::Sync).await?;

        Ok(Self {
            cfd_actor_addr,
            cfd_feed_receiver,
            order_feed_receiver,
            update_cfd_feed_receiver,
            inc_conn_addr,
        })
    }
}

pub struct Taker<O, M> {
    pub cfd_actor_addr: Address<taker_cfd::Actor<O, M>>,
    pub cfd_feed_receiver: watch::Receiver<Vec<Cfd>>,
    pub order_feed_receiver: watch::Receiver<Option<Order>>,
    pub update_cfd_feed_receiver: watch::Receiver<UpdateCfdProposals>,
}

impl<O, M> Taker<O, M>
where
    O: xtra::Handler<oracle::MonitorAttestation>
        + xtra::Handler<oracle::GetAnnouncement>
        + xtra::Handler<oracle::FetchAnnouncement>
        + xtra::Handler<oracle::Sync>,
    M: xtra::Handler<monitor::StartMonitoring>
        + xtra::Handler<monitor::Sync>
        + xtra::Handler<monitor::CollaborativeSettlement>
        + xtra::Handler<oracle::Attestation>,
{
    pub async fn new<F>(
        db: SqlitePool,
        wallet: Wallet,
        oracle_pk: schnorrsig::PublicKey,
        send_to_maker: Box<dyn MessageChannel<wire::TakerToMaker>>,
        read_from_maker: Box<dyn Stream<Item = taker_cfd::MakerStreamMessage> + Unpin + Send>,
        oracle_constructor: impl Fn(Vec<Cfd>, Box<dyn StrongMessageChannel<Attestation>>) -> O,
        monitor_constructor: impl Fn(Box<dyn StrongMessageChannel<monitor::Event>>, Vec<Cfd>) -> F,
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

        let cfd_actor_addr = taker_cfd::Actor::new(
            db,
            wallet,
            oracle_pk,
            cfd_feed_sender,
            order_feed_sender,
            update_cfd_feed_sender,
            send_to_maker,
            monitor_addr.clone(),
            oracle_addr,
        )
        .create(None)
        .spawn_global();

        tokio::spawn(cfd_actor_addr.clone().attach_stream(read_from_maker));

        tokio::spawn(
            monitor_ctx
                .notify_interval(Duration::from_secs(20), || monitor::Sync)
                .map_err(|e| anyhow::anyhow!(e))?,
        );
        tokio::spawn(
            monitor_ctx
                .run(monitor_constructor(Box::new(cfd_actor_addr.clone()), cfds.clone()).await?),
        );

        tokio::spawn(
            oracle_ctx
                .notify_interval(Duration::from_secs(5), || oracle::Sync)
                .unwrap(),
        );
        let fan_out_actor = fan_out::Actor::new(&[&cfd_actor_addr, &monitor_addr])
            .create(None)
            .spawn_global();

        tokio::spawn(oracle_ctx.run(oracle_constructor(cfds, Box::new(fan_out_actor))));

        Ok(Self {
            cfd_actor_addr,
            cfd_feed_receiver,
            order_feed_receiver,
            update_cfd_feed_receiver,
        })
    }
}
