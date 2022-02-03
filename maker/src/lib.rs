use anyhow::Result;
use daemon::bdk;
use daemon::bdk::bitcoin;
use daemon::command;
use daemon::fan_out;
use daemon::maia::secp256k1_zkp::schnorrsig;
use daemon::model::cfd::OrderId;
use daemon::model::cfd::Role;
use daemon::model::FundingRate;
use daemon::model::OpeningFee;
use daemon::model::Price;
use daemon::model::TxFeeRate;
use daemon::model::Usd;
use daemon::monitor;
use daemon::oracle;
use daemon::process_manager;
use daemon::projection;
use daemon::sqlx;
use daemon::wallet;
use std::net::SocketAddr;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::message_channel::StrongMessageChannel;
use xtra::Actor;
use xtra::Address;

mod collab_settlement_maker;
pub mod maker_cfd;
mod maker_inc_connections;
mod rollover_maker;
mod setup_maker;

pub struct ActorSystem<O, W> {
    pub cfd_actor: Address<maker_cfd::Actor<O, maker_inc_connections::Actor, W>>,
    wallet_actor: Address<W>,
    executor: command::Executor,

    _tasks: Tasks,
}

impl<O, W> ActorSystem<O, W>
where
    O: xtra::Handler<oracle::MonitorAttestation>
        + xtra::Handler<oracle::GetAnnouncement>
        + xtra::Handler<oracle::Sync>,
    W: xtra::Handler<wallet::BuildPartyParams>
        + xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::Withdraw>,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new<M>(
        db: sqlx::SqlitePool,
        wallet_addr: Address<W>,
        oracle_pk: schnorrsig::PublicKey,
        oracle_constructor: impl FnOnce(Box<dyn StrongMessageChannel<oracle::Attestation>>) -> O,
        monitor_constructor: impl FnOnce(Box<dyn StrongMessageChannel<monitor::Event>>) -> Result<M>,
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
    {
        let (monitor_addr, monitor_ctx) = xtra::Context::new(None);
        let (oracle_addr, oracle_ctx) = xtra::Context::new(None);
        let (inc_conn_addr, inc_conn_ctx) = xtra::Context::new(None);
        let (process_manager_addr, process_manager_ctx) = xtra::Context::new(None);

        let executor = command::Executor::new(db.clone(), process_manager_addr.clone());

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
            db,
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

        tasks.add(monitor_ctx.run(monitor_constructor(Box::new(cfd_actor_addr.clone()))?));

        let (fan_out_actor, fan_out_actor_fut) =
            fan_out::Actor::new(&[&cfd_actor_addr, &monitor_addr])
                .create(None)
                .run();
        tasks.add(fan_out_actor_fut);

        tasks.add(oracle_ctx.run(oracle_constructor(Box::new(fan_out_actor))));

        tracing::debug!("Maker actor system ready");

        Ok(Self {
            cfd_actor: cfd_actor_addr,
            wallet_actor: wallet_addr,
            executor,
            _tasks: tasks,
        })
    }

    pub async fn new_order(
        &self,
        price: Price,
        min_quantity: Usd,
        max_quantity: Usd,
        fee_rate: Option<TxFeeRate>,
        funding_rate: Option<FundingRate>,
        opening_fee: Option<OpeningFee>,
    ) -> Result<()> {
        self.cfd_actor
            .send(maker_cfd::NewOrder {
                price,
                min_quantity,
                max_quantity,
                tx_fee_rate: fee_rate.unwrap_or_default(),
                funding_rate: funding_rate.unwrap_or_default(),
                opening_fee: opening_fee.unwrap_or_default(),
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
        amount: Option<bitcoin::Amount>,
        address: bitcoin::Address,
        fee: f32,
    ) -> Result<bitcoin::Txid> {
        self.wallet_actor
            .send(wallet::Withdraw {
                amount,
                address,
                fee: Some(bdk::FeeRate::from_sat_per_vb(fee)),
            })
            .await?
    }
}
