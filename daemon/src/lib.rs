#![cfg_attr(not(test), warn(clippy::unwrap_used))]

use crate::bitcoin::Txid;
use anyhow::Context as _;
use anyhow::Result;
use bdk::bitcoin;
use bdk::bitcoin::Amount;
use bdk::FeeRate;
use connection::ConnectionStatus;
use libp2p_core::Multiaddr;
use libp2p_tcp::TokioTcpConfig;
use maia::secp256k1_zkp::schnorrsig;
use model::olivia;
use model::FundingRate;
use model::Identity;
use model::OpeningFee;
use model::Order;
use model::OrderId;
use model::Price;
use model::Role;
use model::TxFeeRate;
use model::Usd;
use seed::Identities;
use std::net::SocketAddr;
use std::time::Duration;
use time::ext::NumericalDuration;
use tokio::sync::watch;
use tokio_tasks::Tasks;
use xtra::prelude::*;
use xtra_bitmex_price_feed::QUOTE_INTERVAL_MINUTES;
use xtra_libp2p::dialer;
use xtra_libp2p::listener;
use xtra_libp2p::Endpoint;
use xtras::supervisor;

pub use bdk;
pub use maia;

pub mod auto_rollover;
mod close_cfds;
pub mod collab_settlement_maker;
pub mod collab_settlement_taker;
pub mod command;
pub mod connection;
pub mod db;
mod future_ext;
pub mod libp2p_utils;
pub mod maker_cfd;
pub mod maker_inc_connections;
pub mod monitor;
mod noise;
pub mod oracle;
pub mod position_metrics;
pub mod process_manager;
pub mod projection;
pub mod rollover_maker;
pub mod rollover_taker;
pub mod seed;
pub mod setup_contract;
pub mod setup_maker;
pub mod setup_taker;
pub mod taker_cfd;
mod transaction_ext;
pub mod version;
pub mod wallet;
pub mod wire;

/// Duration between the heartbeats sent by the maker, used by the taker to
/// determine whether the maker is online.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

pub const ENDPOINT_CONNECTION_TIMEOUT: Duration = Duration::from_secs(20);
pub const PING_INTERVAL: Option<Duration> = Some(Duration::from_secs(5));

pub const N_PAYOUTS: usize = 200;

pub struct MakerActorSystem<O, W> {
    pub cfd_actor: Address<maker_cfd::Actor<O, maker_inc_connections::Actor, W>>,
    wallet_actor: Address<W>,
    _close_cfds_actor: Address<close_cfds::Actor>,
    executor: command::Executor,
    _tasks: Tasks,
    _listener_supervisor: Address<supervisor::Actor<listener::Actor, listener::Error>>,
    _position_metrics_actor: Address<position_metrics::Actor>,
}

impl<O, W> MakerActorSystem<O, W>
where
    O: Handler<oracle::MonitorAttestation>
        + Handler<oracle::GetAnnouncement>
        + Handler<oracle::Sync>
        + Actor<Stop = ()>,
    W: Handler<wallet::BuildPartyParams>
        + Handler<wallet::Sign>
        + Handler<wallet::Withdraw>
        + Actor<Stop = ()>,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new<M>(
        db: db::Connection,
        wallet_addr: Address<W>,
        oracle_pk: schnorrsig::PublicKey,
        oracle_constructor: impl FnOnce(command::Executor) -> O,
        monitor_constructor: impl FnOnce(command::Executor) -> Result<M>,
        settlement_interval: time::Duration,
        n_payouts: usize,
        projection_actor: Address<projection::Actor>,
        identity: Identities,
        heartbeat_interval: Duration,
        p2p_socket: SocketAddr,
    ) -> Result<Self>
    where
        M: Handler<monitor::StartMonitoring>
            + Handler<monitor::Sync>
            + Handler<monitor::MonitorCollaborativeSettlement>
            + Handler<monitor::TryBroadcastTransaction>
            + Handler<monitor::MonitorCetFinality>
            + Actor<Stop = ()>,
    {
        let (monitor_addr, monitor_ctx) = Context::new(None);
        let (oracle_addr, oracle_ctx) = Context::new(None);
        let (inc_conn_addr, inc_conn_ctx) = Context::new(None);
        let (process_manager_addr, process_manager_ctx) = Context::new(None);

        let executor = command::Executor::new(db.clone(), process_manager_addr.clone());

        let mut tasks = Tasks::default();

        tasks.add(process_manager_ctx.run(process_manager::Actor::new(
            db.clone(),
            Role::Maker,
            &projection_actor,
            &monitor_addr,
            &monitor_addr,
            &monitor_addr,
            &monitor_addr,
            &oracle_addr,
        )));

        let cfd_actor_addr = maker_cfd::Actor::new(
            db.clone(),
            wallet_addr.clone(),
            settlement_interval,
            oracle_pk,
            projection_actor,
            process_manager_addr,
            inc_conn_addr,
            oracle_addr,
            n_payouts,
        )
        .create(None)
        .spawn(&mut tasks);

        let (endpoint_addr, endpoint_context) = Context::new(None);

        let ping_address = xtra_libp2p_ping::Actor::new(endpoint_addr.clone(), PING_INTERVAL)
            .create(None)
            .spawn(&mut tasks);

        let endpoint = Endpoint::new(
            TokioTcpConfig::new(),
            identity.libp2p,
            ENDPOINT_CONNECTION_TIMEOUT,
            [(
                xtra_libp2p_ping::PROTOCOL_NAME,
                xtra::message_channel::StrongMessageChannel::clone_channel(&ping_address),
            )],
        );

        tasks.add(endpoint_context.run(endpoint));

        let libp2p_socket = libp2p_utils::libp2p_socket_from_legacy_networking(&p2p_socket);
        let endpoint_listen =
            libp2p_utils::create_listen_tcp_multiaddr(&libp2p_socket).expect("to parse properly");

        let (supervisor, _listener_actor) = supervisor::Actor::with_policy(
            move || {
                let endpoint_addr = endpoint_addr.clone();
                let endpoint_listen = endpoint_listen.clone();
                listener::Actor::new(endpoint_addr, endpoint_listen)
            },
            |_: &listener::Error| true, // always restart listener actor
        );
        let listener_supervisor = supervisor.create(None).spawn(&mut tasks);

        tasks.add(inc_conn_ctx.run(maker_inc_connections::Actor::new(
            Box::new(cfd_actor_addr.clone()),
            Box::new(cfd_actor_addr.clone()),
            Box::new(cfd_actor_addr.clone()),
            identity.identity_sk,
            heartbeat_interval,
            p2p_socket,
        )));

        tasks.add(monitor_ctx.run(monitor_constructor(executor.clone())?));

        tasks.add(oracle_ctx.run(oracle_constructor(executor.clone())));

        let close_cfds_actor = close_cfds::Actor::new(db.clone())
            .create(None)
            .spawn(&mut tasks);

        let position_metrics_actor = position_metrics::Actor::new(db)
            .create(None)
            .spawn(&mut tasks);

        tracing::debug!("Maker actor system ready");

        Ok(Self {
            cfd_actor: cfd_actor_addr,
            wallet_actor: wallet_addr,
            _close_cfds_actor: close_cfds_actor,
            executor,
            _tasks: tasks,
            _listener_supervisor: listener_supervisor,
            _position_metrics_actor: position_metrics_actor,
        })
    }

    /// Adjust the parameters which create offers for the connected takers.
    ///
    /// Once one offer is taken, another one with the same parameters is created.
    #[allow(clippy::too_many_arguments)]
    pub async fn set_offer_params(
        &self,
        price_long: Option<Price>,
        price_short: Option<Price>,
        min_quantity: Usd,
        max_quantity: Usd,
        tx_fee_rate: TxFeeRate,
        funding_rate_long: FundingRate,
        funding_rate_short: FundingRate,
        opening_fee: OpeningFee,
    ) -> Result<()> {
        self.cfd_actor
            .send(maker_cfd::OfferParams {
                price_long,
                price_short,
                min_quantity,
                max_quantity,
                tx_fee_rate,
                funding_rate_long,
                funding_rate_short,
                opening_fee,
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
        self.executor
            .execute(order_id, |cfd| cfd.manual_commit_to_blockchain())
            .await?;

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

pub struct TakerActorSystem<O, W, P> {
    pub cfd_actor: Address<taker_cfd::Actor<O, W>>,
    pub connection_actor: Address<connection::Actor>,
    wallet_actor: Address<W>,
    pub auto_rollover_actor: Address<auto_rollover::Actor<O>>,
    pub price_feed_actor: Address<P>,
    executor: command::Executor,
    /// Keep this one around to avoid the supervisor being dropped due to ref-count changes on the
    /// address.
    _price_feed_supervisor: Address<supervisor::Actor<P, xtra_bitmex_price_feed::Error>>,
    _dialer_actor: Address<dialer::Actor>,
    _dialer_supervisor: Address<supervisor::Actor<dialer::Actor, dialer::Error>>,
    _close_cfds_actor: Address<close_cfds::Actor>,

    pub maker_online_status_feed_receiver: watch::Receiver<ConnectionStatus>,

    _tasks: Tasks,
}

impl<O, W, P> TakerActorSystem<O, W, P>
where
    O: Handler<oracle::MonitorAttestation>
        + Handler<oracle::GetAnnouncement>
        + Handler<oracle::Sync>
        + Actor<Stop = ()>,
    W: Handler<wallet::BuildPartyParams>
        + Handler<wallet::Sign>
        + Handler<wallet::Withdraw>
        + Actor<Stop = ()>,
    P: Handler<xtra_bitmex_price_feed::LatestQuote> + Actor<Stop = xtra_bitmex_price_feed::Error>,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new<M>(
        db: db::Connection,
        wallet_actor_addr: Address<W>,
        oracle_pk: schnorrsig::PublicKey,
        identity: Identities,
        oracle_constructor: impl FnOnce(command::Executor) -> O,
        monitor_constructor: impl FnOnce(command::Executor) -> Result<M>,
        price_feed_constructor: impl (Fn() -> P) + Send + 'static,
        n_payouts: usize,
        maker_heartbeat_interval: Duration,
        connect_timeout: Duration,
        projection_actor: Address<projection::Actor>,
        maker_identity: Identity,
        maker_multiaddr: Multiaddr,
    ) -> Result<Self>
    where
        M: Handler<monitor::StartMonitoring>
            + Handler<monitor::Sync>
            + Handler<monitor::MonitorCollaborativeSettlement>
            + Handler<monitor::MonitorCetFinality>
            + Handler<monitor::TryBroadcastTransaction>
            + Actor<Stop = ()>,
    {
        let (maker_online_status_feed_sender, maker_online_status_feed_receiver) =
            watch::channel(ConnectionStatus::Offline { reason: None });

        let (monitor_addr, monitor_ctx) = Context::new(None);
        let (oracle_addr, oracle_ctx) = Context::new(None);
        let (process_manager_addr, process_manager_ctx) = Context::new(None);

        let executor = command::Executor::new(db.clone(), process_manager_addr.clone());

        let mut tasks = Tasks::default();

        tasks.add(process_manager_ctx.run(process_manager::Actor::new(
            db.clone(),
            Role::Taker,
            &projection_actor,
            &monitor_addr,
            &monitor_addr,
            &monitor_addr,
            &monitor_addr,
            &oracle_addr,
        )));

        let (connection_actor_addr, connection_actor_ctx) = Context::new(None);
        let cfd_actor_addr = taker_cfd::Actor::new(
            db.clone(),
            wallet_actor_addr.clone(),
            oracle_pk,
            projection_actor,
            process_manager_addr.clone(),
            connection_actor_addr.clone(),
            oracle_addr.clone(),
            n_payouts,
            maker_identity,
        )
        .create(None)
        .spawn(&mut tasks);

        let auto_rollover_addr = auto_rollover::Actor::new(
            db.clone(),
            oracle_pk,
            process_manager_addr,
            connection_actor_addr.clone(),
            oracle_addr,
            n_payouts,
        )
        .create(None)
        .spawn(&mut tasks);

        // Timeout happens when taker did not receive two consecutive heartbeats
        let taker_heartbeat_timeout = maker_heartbeat_interval
            .checked_mul(2)
            .expect("not to overflow");

        tasks.add(connection_actor_ctx.run(connection::Actor::new(
            maker_online_status_feed_sender,
            &cfd_actor_addr,
            identity.identity_sk,
            taker_heartbeat_timeout,
            connect_timeout,
        )));

        tasks.add(monitor_ctx.run(monitor_constructor(executor.clone())?));

        tasks.add(oracle_ctx.run(oracle_constructor(executor.clone())));

        let (endpoint_addr, endpoint_context) = Context::new(None);

        let ping_address = xtra_libp2p_ping::Actor::new(endpoint_addr.clone(), PING_INTERVAL)
            .create(None)
            .spawn(&mut tasks);

        let endpoint = Endpoint::new(
            TokioTcpConfig::new(),
            identity.libp2p,
            ENDPOINT_CONNECTION_TIMEOUT,
            [(
                xtra_libp2p_ping::PROTOCOL_NAME,
                xtra::message_channel::StrongMessageChannel::clone_channel(&ping_address),
            )],
        );

        tasks.add(endpoint_context.run(endpoint));

        let dialer_constructor =
            { move || dialer::Actor::new(endpoint_addr.clone(), maker_multiaddr.clone()) };

        let (supervisor, dialer_actor) = supervisor::Actor::with_policy(
            dialer_constructor,
            |_: &dialer::Error| true, // always restart dialer actor
        );
        let dialer_supervisor = supervisor.create(None).spawn(&mut tasks);

        let (supervisor, price_feed_actor) = supervisor::Actor::with_policy(
            price_feed_constructor,
            |_| true, // always restart price feed actor
        );

        let price_feed_supervisor = supervisor.create(None).spawn(&mut tasks);

        let close_cfds_actor = close_cfds::Actor::new(db).create(None).spawn(&mut tasks);

        tracing::debug!("Taker actor system ready");

        Ok(Self {
            cfd_actor: cfd_actor_addr,
            connection_actor: connection_actor_addr,
            wallet_actor: wallet_actor_addr,
            auto_rollover_actor: auto_rollover_addr,
            price_feed_actor,
            executor,
            _price_feed_supervisor: price_feed_supervisor,
            _dialer_actor: dialer_actor,
            _dialer_supervisor: dialer_supervisor,
            _close_cfds_actor: close_cfds_actor,
            _tasks: tasks,
            maker_online_status_feed_receiver,
        })
    }

    pub async fn take_offer(&self, order_id: OrderId, quantity: Usd) -> Result<()> {
        self.cfd_actor
            .send(taker_cfd::TakeOffer { order_id, quantity })
            .await??;
        Ok(())
    }

    pub async fn commit(&self, order_id: OrderId) -> Result<()> {
        self.executor
            .execute(order_id, |cfd| cfd.manual_commit_to_blockchain())
            .await?;

        Ok(())
    }

    pub async fn propose_settlement(&self, order_id: OrderId) -> Result<()> {
        let latest_quote = self
            .price_feed_actor
            .send(xtra_bitmex_price_feed::LatestQuote)
            .await
            .context("Price feed not available")?
            .context("No quote available")?;

        let quote_timestamp = latest_quote
            .timestamp
            .format(&time::format_description::well_known::Rfc3339)
            .context("Failed to format timestamp")?;

        let threshold = QUOTE_INTERVAL_MINUTES.minutes() * 2;

        if latest_quote.is_older_than(threshold) {
            anyhow::bail!(
                "Latest quote is older than {} minutes. Refusing to settle with old price.",
                threshold.whole_minutes()
            )
        }

        self.cfd_actor
            .send(taker_cfd::ProposeSettlement {
                order_id,
                bid: Price::new(latest_quote.bid())?,
                ask: Price::new(latest_quote.ask())?,
                quote_timestamp,
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
