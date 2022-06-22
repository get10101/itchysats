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
use maia_core::secp256k1_zkp::schnorrsig;
use model::libp2p::PeerId;
use model::olivia;
use model::Identity;
use model::Leverage;
use model::Order;
use model::OrderId;
use model::Price;
use model::Role;
use model::Usd;
use parse_display::Display;
use seed::Identities;
use std::time::Duration;
use time::ext::NumericalDuration;
use tokio::sync::watch;
use tokio_tasks::Tasks;
use xtra::prelude::*;
use xtra_bitmex_price_feed::QUOTE_INTERVAL_MINUTES;
use xtra_libp2p::dialer;
use xtra_libp2p::endpoint;
use xtra_libp2p::multiaddress_ext::MultiaddrExt;
use xtra_libp2p::Endpoint;
use xtra_libp2p_ping::ping;
use xtra_libp2p_ping::pong;
use xtras::supervisor;
use xtras::supervisor::always_restart;
use xtras::supervisor::always_restart_after;

pub use bdk;
pub use maia;
pub use maia_core;

pub mod archive_closed_cfds;
pub mod archive_failed_cfds;
pub mod auto_rollover;
pub mod collab_settlement;
pub mod command;
pub mod connection;
mod future_ext;
pub mod libp2p_utils;
pub mod monitor;
pub mod noise;
pub mod oracle;
pub mod position_metrics;
pub mod process_manager;
pub mod projection;
pub mod rollover;
pub mod seed;
pub mod setup_contract;
// TODO: Remove setup_contract_deprecated module after phasing out legacy networking
pub mod setup_contract_deprecated;
pub mod setup_taker;
pub mod shared_protocol;
pub mod taker_cfd;
mod transaction_ext;
pub mod version;
pub mod wallet;
pub mod wire;

/// Duration between the heartbeats sent by the maker, used by the taker to
/// determine whether the maker is online.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// Duration between the restart attempts after a supervised actor has quit with
/// a failure.
pub const RESTART_INTERVAL: Duration = Duration::from_secs(5);

pub const ENDPOINT_CONNECTION_TIMEOUT: Duration = Duration::from_secs(20);
pub const PING_INTERVAL: Duration = Duration::from_secs(5);

pub const N_PAYOUTS: usize = 200;

pub struct TakerActorSystem<O, W, P> {
    pub cfd_actor: Address<taker_cfd::Actor<O, W>>,
    pub connection_actor: Address<connection::Actor>,
    wallet_actor: Address<W>,
    pub auto_rollover_actor: Address<auto_rollover::Actor>,
    pub price_feed_actor: Address<P>,
    executor: command::Executor,
    /// Keep this one around to avoid the supervisor being dropped due to ref-count changes on the
    /// address.
    _price_feed_supervisor: Address<supervisor::Actor<P, xtra_bitmex_price_feed::Error>>,
    _collab_settlement_supervisor:
        Address<supervisor::Actor<collab_settlement::taker::Actor, supervisor::UnitReason>>,
    _rollover_supervisor:
        Address<supervisor::Actor<rollover::taker::Actor, supervisor::UnitReason>>,
    _dialer_supervisor: Address<supervisor::Actor<dialer::Actor, dialer::Error>>,
    _offers_supervisor:
        Address<supervisor::Actor<xtra_libp2p_offer::taker::Actor, supervisor::UnitReason>>,
    _ping_supervisor: Address<supervisor::Actor<ping::Actor, supervisor::UnitReason>>,
    _close_cfds_actor: Address<archive_closed_cfds::Actor>,
    _archive_failed_cfds_actor: Address<archive_failed_cfds::Actor>,
    _pong_actor: Address<pong::Actor>,

    pub maker_online_status_feed_receiver: watch::Receiver<ConnectionStatus>,

    _tasks: Tasks,
}

impl<O, W, P> TakerActorSystem<O, W, P>
where
    O: Handler<oracle::MonitorAttestation> + Handler<oracle::GetAnnouncement> + Actor<Stop = ()>,
    W: Handler<wallet::BuildPartyParams>
        + Handler<wallet::Sign>
        + Handler<wallet::Withdraw>
        + Handler<wallet::Sync>
        + Actor<Stop = ()>,
    P: Handler<xtra_bitmex_price_feed::LatestQuote> + Actor<Stop = xtra_bitmex_price_feed::Error>,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new<M>(
        db: sqlite_db::Connection,
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
        environment: Environment,
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

        let position_metrics_actor = position_metrics::Actor::new(db.clone())
            .create(None)
            .spawn(&mut tasks);

        tasks.add(process_manager_ctx.run(process_manager::Actor::new(
            db.clone(),
            Role::Taker,
            &projection_actor,
            &position_metrics_actor,
            &monitor_addr,
            &monitor_addr,
            &monitor_addr,
            &monitor_addr,
            &oracle_addr,
        )));

        let (endpoint_addr, endpoint_context) = Context::new(None);

        let (collab_settlement_supervisor, libp2p_collab_settlement_addr) =
            supervisor::Actor::new({
                let endpoint_addr = endpoint_addr.clone();
                let executor = executor.clone();
                move || {
                    collab_settlement::taker::Actor::new(
                        endpoint_addr.clone(),
                        executor.clone(),
                        n_payouts,
                    )
                }
            });
        let collab_settlement_supervisor =
            collab_settlement_supervisor.create(None).spawn(&mut tasks);

        let (connection_actor_addr, connection_actor_ctx) = Context::new(None);
        let cfd_actor_addr = taker_cfd::Actor::new(
            db.clone(),
            wallet_actor_addr.clone(),
            oracle_pk,
            projection_actor,
            process_manager_addr,
            connection_actor_addr.clone(),
            oracle_addr.clone(),
            libp2p_collab_settlement_addr,
            n_payouts,
            maker_identity,
            PeerId::from(
                maker_multiaddr
                    .clone()
                    .extract_peer_id()
                    .context("Unable to extract peer id from maker address")?,
            ),
        )
        .create(None)
        .spawn(&mut tasks);

        let (rollover_supervisor, libp2p_rollover_addr) = supervisor::Actor::new({
            let endpoint_addr = endpoint_addr.clone();
            let executor = executor.clone();
            move || {
                rollover::taker::Actor::new(
                    endpoint_addr.clone(),
                    executor.clone(),
                    oracle_pk,
                    xtra::message_channel::MessageChannel::clone_channel(&oracle_addr),
                    n_payouts,
                )
            }
        });
        let rollover_supervisor = rollover_supervisor.create(None).spawn(&mut tasks);

        let auto_rollover_addr = auto_rollover::Actor::new(db.clone(), libp2p_rollover_addr)
            .create(None)
            .spawn(&mut tasks);

        tasks.add(
            connection_actor_ctx
                .with_handler_timeout(Duration::from_secs(120))
                .run(connection::Actor::new(
                    maker_online_status_feed_sender,
                    identity.identity_sk.clone(),
                    identity.peer_id(),
                    maker_heartbeat_interval,
                    connect_timeout,
                    environment,
                )),
        );

        tasks.add(monitor_ctx.run(monitor_constructor(executor.clone())?));
        tasks.add(oracle_ctx.run(oracle_constructor(executor.clone())));

        let dialer_constructor = {
            let endpoint_addr = endpoint_addr.clone();
            move || dialer::Actor::new(endpoint_addr.clone(), maker_multiaddr.clone())
        };
        let (dialer_supervisor, dialer_actor) = supervisor::Actor::with_policy(
            dialer_constructor,
            always_restart_after(RESTART_INTERVAL),
        );

        let (offers_supervisor, libp2p_offer_addr) = supervisor::Actor::new({
            let cfd_actor_addr = cfd_actor_addr.clone();
            move || xtra_libp2p_offer::taker::Actor::new(&cfd_actor_addr)
        });

        let pong_address = pong::Actor::default().create(None).spawn(&mut tasks);
        let endpoint = Endpoint::new(
            Box::new(TokioTcpConfig::new),
            identity.libp2p,
            ENDPOINT_CONNECTION_TIMEOUT,
            [
                (
                    xtra_libp2p_ping::PROTOCOL_NAME,
                    xtra::message_channel::StrongMessageChannel::clone_channel(&pong_address),
                ),
                (
                    xtra_libp2p_offer::PROTOCOL_NAME,
                    xtra::message_channel::StrongMessageChannel::clone_channel(&libp2p_offer_addr),
                ),
            ],
            endpoint::Subscribers::new(
                vec![],
                vec![xtra::message_channel::MessageChannel::clone_channel(
                    &dialer_actor,
                )],
                vec![],
                vec![],
            ),
        );

        tasks.add(endpoint_context.run(endpoint));

        let (supervisor, _ping_address) = supervisor::Actor::new({
            move || ping::Actor::new(endpoint_addr.clone(), PING_INTERVAL)
        });
        let ping_supervisor = supervisor.create(None).spawn(&mut tasks);

        let dialer_supervisor = dialer_supervisor.create(None).spawn(&mut tasks);
        let offers_supervisor = offers_supervisor.create(None).spawn(&mut tasks);

        let (supervisor, price_feed_actor) =
            supervisor::Actor::with_policy(price_feed_constructor, always_restart());

        let price_feed_supervisor = supervisor.create(None).spawn(&mut tasks);

        let close_cfds_actor = archive_closed_cfds::Actor::new(db.clone())
            .create(None)
            .spawn(&mut tasks);
        let archive_failed_cfds_actor = archive_failed_cfds::Actor::new(db)
            .create(None)
            .spawn(&mut tasks);

        tracing::debug!("Taker actor system ready");

        Ok(Self {
            cfd_actor: cfd_actor_addr,
            connection_actor: connection_actor_addr,
            wallet_actor: wallet_actor_addr,
            auto_rollover_actor: auto_rollover_addr,
            price_feed_actor,
            executor,
            _price_feed_supervisor: price_feed_supervisor,
            _rollover_supervisor: rollover_supervisor,
            _collab_settlement_supervisor: collab_settlement_supervisor,
            _dialer_supervisor: dialer_supervisor,
            _offers_supervisor: offers_supervisor,
            _ping_supervisor: ping_supervisor,
            _close_cfds_actor: close_cfds_actor,
            _archive_failed_cfds_actor: archive_failed_cfds_actor,
            _tasks: tasks,
            maker_online_status_feed_receiver,
            _pong_actor: pong_address,
        })
    }

    pub async fn take_offer(
        &self,
        order_id: OrderId,
        quantity: Usd,
        leverage: Leverage,
    ) -> Result<()> {
        self.cfd_actor
            .send(taker_cfd::TakeOffer {
                order_id,
                quantity,
                leverage,
            })
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

    pub async fn sync_wallet(&self) -> Result<()> {
        self.wallet_actor.send(wallet::Sync).await?;
        Ok(())
    }
}

#[derive(Debug, Copy, Clone, Display)]
pub enum Environment {
    Umbrel,
    RaspiBlitz,
    Docker,
    Binary,
    Test,
    Legacy,
    Unknown,
}

impl Environment {
    pub fn from_str_or_unknown(s: &str) -> Environment {
        match s {
            "umbrel" => Environment::Umbrel,
            "raspiblitz" => Environment::RaspiBlitz,
            "docker" => Environment::Docker,
            _ => Environment::Unknown,
        }
    }
}
