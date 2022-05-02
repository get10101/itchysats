use crate::cfd;
use crate::connection;
use anyhow::Result;
use bdk::bitcoin;
use bdk::bitcoin::Amount;
use bdk::bitcoin::Txid;
use daemon::archive_closed_cfds;
use daemon::archive_failed_cfds;
use daemon::command;
use daemon::db;
use daemon::monitor;
use daemon::oracle;
use daemon::position_metrics;
use daemon::process_manager;
use daemon::projection;
use daemon::seed::Identities;
use daemon::wallet;
use libp2p_tcp::TokioTcpConfig;
use maia::secp256k1_zkp::schnorrsig;
use model::FundingRate;
use model::OpeningFee;
use model::OrderId;
use model::Price;
use model::Role;
use model::TxFeeRate;
use model::Usd;
use std::net::SocketAddr;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::Actor;
use xtra::Address;
use xtra::Context;
use xtra::Handler;
use xtra_libp2p::listener;
use xtra_libp2p::Endpoint;
use xtras::supervisor;

const ENDPOINT_CONNECTION_TIMEOUT: Duration = Duration::from_secs(20);
const PING_INTERVAL: Option<Duration> = Some(Duration::from_secs(5));

pub struct ActorSystem<O, W> {
    pub cfd_actor: Address<cfd::Actor<O, connection::Actor, W>>,
    wallet_actor: Address<W>,
    _archive_closed_cfds_actor: Address<archive_closed_cfds::Actor>,
    _archive_failed_cfds_actor: Address<archive_failed_cfds::Actor>,
    executor: command::Executor,
    _tasks: Tasks,
    _listener_supervisor: Address<supervisor::Actor<listener::Actor, listener::Error>>,
    _position_metrics_actor: Address<position_metrics::Actor>,
}

impl<O, W> ActorSystem<O, W>
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

        let position_metrics_actor = position_metrics::Actor::new(db.clone())
            .create(None)
            .spawn(&mut tasks);

        tasks.add(process_manager_ctx.run(process_manager::Actor::new(
            db.clone(),
            Role::Maker,
            &projection_actor,
            &position_metrics_actor,
            &monitor_addr,
            &monitor_addr,
            &monitor_addr,
            &monitor_addr,
            &oracle_addr,
        )));

        let cfd_actor_addr = cfd::Actor::new(
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

        let libp2p_socket = daemon::libp2p_utils::libp2p_socket_from_legacy_networking(&p2p_socket);
        let endpoint_listen = daemon::libp2p_utils::create_listen_tcp_multiaddr(&libp2p_socket)
            .expect("to parse properly");

        let (supervisor, _listener_actor) = supervisor::Actor::with_policy(
            move || {
                let endpoint_addr = endpoint_addr.clone();
                let endpoint_listen = endpoint_listen.clone();
                listener::Actor::new(endpoint_addr, endpoint_listen)
            },
            |_: &listener::Error| true, // always restart listener actor
        );
        let listener_supervisor = supervisor.create(None).spawn(&mut tasks);

        tasks.add(inc_conn_ctx.run(connection::Actor::new(
            Box::new(cfd_actor_addr.clone()),
            Box::new(cfd_actor_addr.clone()),
            Box::new(cfd_actor_addr.clone()),
            identity.identity_sk,
            heartbeat_interval,
            p2p_socket,
        )));

        tasks.add(monitor_ctx.run(monitor_constructor(executor.clone())?));

        tasks.add(oracle_ctx.run(oracle_constructor(executor.clone())));

        let archive_closed_cfds_actor = archive_closed_cfds::Actor::new(db.clone())
            .create(None)
            .spawn(&mut tasks);

        let archive_failed_cfds_actor = archive_failed_cfds::Actor::new(db)
            .create(None)
            .spawn(&mut tasks);

        tracing::debug!("Maker actor system ready");

        Ok(Self {
            cfd_actor: cfd_actor_addr,
            wallet_actor: wallet_addr,
            _archive_closed_cfds_actor: archive_closed_cfds_actor,
            _archive_failed_cfds_actor: archive_failed_cfds_actor,
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
            .send(cfd::OfferParams {
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
        self.cfd_actor.send(cfd::AcceptOrder { order_id }).await??;
        Ok(())
    }

    pub async fn reject_order(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor.send(cfd::RejectOrder { order_id }).await??;
        Ok(())
    }

    pub async fn accept_settlement(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor
            .send(cfd::AcceptSettlement { order_id })
            .await??;
        Ok(())
    }

    pub async fn reject_settlement(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor
            .send(cfd::RejectSettlement { order_id })
            .await??;
        Ok(())
    }

    pub async fn accept_rollover(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor
            .send(cfd::AcceptRollover { order_id })
            .await??;
        Ok(())
    }

    pub async fn reject_rollover(&self, order_id: OrderId) -> Result<()> {
        self.cfd_actor
            .send(cfd::RejectRollover { order_id })
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