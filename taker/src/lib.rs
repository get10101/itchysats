use anyhow::Context;
use anyhow::Result;
use connection::ConnectionStatus;
use daemon::bdk::bitcoin;
use daemon::bdk::FeeRate;
use daemon::command;
use daemon::fan_out;
use daemon::maia::secp256k1_zkp::schnorrsig;
use daemon::model::cfd::OrderId;
use daemon::model::cfd::Role;
use daemon::model::Identity;
use daemon::model::Price;
use daemon::model::Usd;
use daemon::monitor;
use daemon::oracle;
use daemon::process_manager;
use daemon::projection;
use daemon::sqlx;
use daemon::wallet;
use std::time::Duration;
use time::ext::NumericalDuration;
use tokio::sync::watch;
use tokio_tasks::Tasks;
use xtra::message_channel::StrongMessageChannel;
use xtra::Actor as _;
use xtra::Address;
use xtra_bitmex_price_feed::QUOTE_INTERVAL_MINUTES;
use xtras::supervisor;

pub mod auto_rollover;
mod collab_settlement_taker;
pub mod connection;
mod rollover_taker;
mod setup_taker;
mod taker_cfd;
pub mod to_sse_event;

pub struct ActorSystem<O, W, P> {
    pub cfd_actor: Address<taker_cfd::Actor<O, W>>,
    pub connection_actor: Address<connection::Actor>,
    wallet_actor: Address<W>,
    pub auto_rollover_actor: Address<auto_rollover::Actor<O>>,
    pub price_feed_actor: Address<P>,
    executor: command::Executor,
    /// Keep this one around to avoid the supervisor being dropped due to ref-count changes on the
    /// address.
    _price_feed_supervisor: Address<supervisor::Actor<P, xtra_bitmex_price_feed::Error>>,

    pub maker_online_status_feed_receiver: watch::Receiver<ConnectionStatus>,

    _tasks: Tasks,
}

impl<O, W, P> ActorSystem<O, W, P>
where
    O: xtra::Handler<oracle::MonitorAttestation>
        + xtra::Handler<oracle::GetAnnouncement>
        + xtra::Handler<oracle::Sync>,
    W: xtra::Handler<wallet::BuildPartyParams>
        + xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::Withdraw>,
    P: xtra::Handler<xtra_bitmex_price_feed::LatestQuote>,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new<M>(
        db: sqlx::SqlitePool,
        wallet_actor_addr: Address<W>,
        oracle_pk: schnorrsig::PublicKey,
        identity_sk: x25519_dalek::StaticSecret,
        oracle_constructor: impl FnOnce(Box<dyn StrongMessageChannel<oracle::Attestation>>) -> O,
        monitor_constructor: impl FnOnce(Box<dyn StrongMessageChannel<monitor::Event>>) -> Result<M>,
        price_feed_constructor: impl (Fn(Address<supervisor::Actor<P, xtra_bitmex_price_feed::Error>>) -> P)
            + Send
            + 'static,
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
    {
        let (maker_online_status_feed_sender, maker_online_status_feed_receiver) =
            watch::channel(ConnectionStatus::Offline { reason: None });

        let (monitor_addr, monitor_ctx) = xtra::Context::new(None);
        let (oracle_addr, oracle_ctx) = xtra::Context::new(None);
        let (process_manager_addr, process_manager_ctx) = xtra::Context::new(None);

        let executor = command::Executor::new(db.clone(), process_manager_addr.clone());

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
            projection_actor,
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

        tasks.add(monitor_ctx.run(monitor_constructor(Box::new(cfd_actor_addr.clone()))?));

        let (fan_out_actor, fan_out_actor_fut) =
            fan_out::Actor::new(&[&cfd_actor_addr, &monitor_addr])
                .create(None)
                .run();

        tasks.add(fan_out_actor_fut);

        tasks.add(oracle_ctx.run(oracle_constructor(Box::new(fan_out_actor))));

        let (supervisor, price_feed_actor) = supervisor::Actor::new(
            price_feed_constructor,
            |_| true, // always restart price feed actor
        );

        let (price_feed_supervisor, supervisor_fut) = supervisor.create(None).run();
        tasks.add(supervisor_fut);

        tracing::debug!("Taker actor system ready");

        Ok(Self {
            cfd_actor: cfd_actor_addr,
            connection_actor: connection_actor_addr,
            wallet_actor: wallet_actor_addr,
            auto_rollover_actor: auto_rollover_addr,
            price_feed_actor,
            executor,
            _price_feed_supervisor: price_feed_supervisor,
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

        if latest_quote.is_older_than(QUOTE_INTERVAL_MINUTES.minutes()) {
            anyhow::bail!(
                "Latest quote is older than {} minutes. Refusing to settle with old price.",
                QUOTE_INTERVAL_MINUTES
            )
        }

        self.cfd_actor
            .send(taker_cfd::ProposeSettlement {
                order_id,
                current_price: Price::new(latest_quote.for_taker())?,
            })
            .await?
    }

    pub async fn withdraw(
        &self,
        amount: Option<bitcoin::Amount>,
        address: bitcoin::Address,
        fee_rate: FeeRate,
    ) -> Result<bitcoin::Txid> {
        self.wallet_actor
            .send(wallet::Withdraw {
                amount,
                address,
                fee: Some(fee_rate),
            })
            .await?
    }
}
