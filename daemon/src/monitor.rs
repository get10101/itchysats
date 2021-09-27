use crate::actors::log_error;
use crate::model::cfd::{Cfd, OrderId};
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::{PublicKey, Script, Txid};
use bdk::descriptor::Descriptor;
use bdk::miniscript::DescriptorTrait;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use subscription::Monitor;

mod subscription;

const FINALITY_CONFIRMATIONS: u32 = 1;

#[derive(Clone)]
pub struct MonitorParams {
    pub lock: (Txid, Descriptor<PublicKey>),
    pub commit: (Txid, Descriptor<PublicKey>),
    pub cets: Vec<(Txid, Script, RangeInclusive<u64>)>,
    pub refund: (Txid, Script, u32),
}

impl<T> Actor<T>
where
    T: xtra::Actor + xtra::Handler<CfdMonitoringEvent>,
{
    pub fn new(
        electrum_rpc_url: &str,
        cfds: HashMap<OrderId, MonitorParams>,
        cfd_actor_addr: xtra::Address<T>,
    ) -> Self {
        let monitor = Monitor::new(electrum_rpc_url, FINALITY_CONFIRMATIONS).unwrap();

        Self {
            monitor,
            cfds,
            cfd_actor_addr,
        }
    }

    async fn handle_start_monitoring(&mut self, msg: StartMonitoring) -> Result<()> {
        let StartMonitoring { id, params } = msg;

        self.cfds.insert(id, params.clone());

        tokio::spawn({
            let cfd_actor_addr = self.cfd_actor_addr.clone();
            let lock_subscription = self
                .monitor
                .subscribe_to((params.lock.0, params.lock.1.script_pubkey()))
                .await;
            async move {
                lock_subscription.wait_until_final().await.unwrap();

                cfd_actor_addr
                    .do_send_async(CfdMonitoringEvent::LockFinality(id))
                    .await
                    .unwrap();
            }
        });

        let commit_subscription = self
            .monitor
            .subscribe_to((params.commit.0, params.commit.1.script_pubkey()))
            .await;

        tokio::spawn({
            let cfd_actor_addr = self.cfd_actor_addr.clone();
            let commit_subscription = commit_subscription.clone();
            async move {
                commit_subscription.wait_until_final().await.unwrap();

                cfd_actor_addr
                    .do_send_async(CfdMonitoringEvent::CommitFinality(id))
                    .await
                    .unwrap();
            }
        });

        tokio::spawn({
            let cfd_actor_addr = self.cfd_actor_addr.clone();
            let commit_subscription = commit_subscription.clone();
            async move {
                commit_subscription
                    .wait_until_confirmed_with(Cfd::CET_TIMELOCK)
                    .await
                    .unwrap();

                cfd_actor_addr
                    .do_send_async(CfdMonitoringEvent::CetTimelockExpired(id))
                    .await
                    .unwrap();
            }
        });

        tokio::spawn({
            let cfd_actor_addr = self.cfd_actor_addr.clone();
            let commit_subscription = commit_subscription.clone();
            let refund_timelock = params.refund.2;
            async move {
                commit_subscription
                    .wait_until_confirmed_with(refund_timelock)
                    .await
                    .unwrap();

                cfd_actor_addr
                    .do_send_async(CfdMonitoringEvent::RefundTimelockExpired(id))
                    .await
                    .unwrap();
            }
        });

        tokio::spawn({
            let cfd_actor_addr = self.cfd_actor_addr.clone();
            let refund_subscription = self
                .monitor
                .subscribe_to((params.refund.0, params.refund.1))
                .await;
            async move {
                refund_subscription.wait_until_final().await.unwrap();

                cfd_actor_addr
                    .do_send_async(CfdMonitoringEvent::RefundFinality(id))
                    .await
                    .unwrap();
            }
        });

        // TODO: CET subscription => Requires information from Oracle

        Ok(())
    }
}

pub struct StartMonitoring {
    pub id: OrderId,
    pub params: MonitorParams,
}

impl xtra::Message for StartMonitoring {
    type Result = ();
}

#[derive(Debug, Clone)]
pub enum CfdMonitoringEvent {
    LockFinality(OrderId),
    CommitFinality(OrderId),
    CetTimelockExpired(OrderId),
    RefundTimelockExpired(OrderId),
    RefundFinality(OrderId),
}

impl CfdMonitoringEvent {
    pub fn order_id(&self) -> OrderId {
        let order_id = match self {
            CfdMonitoringEvent::LockFinality(order_id) => order_id,
            CfdMonitoringEvent::CommitFinality(order_id) => order_id,
            CfdMonitoringEvent::CetTimelockExpired(order_id) => order_id,
            CfdMonitoringEvent::RefundTimelockExpired(order_id) => order_id,
            CfdMonitoringEvent::RefundFinality(order_id) => order_id,
        };

        *order_id
    }
}

impl xtra::Message for CfdMonitoringEvent {
    type Result = ();
}

pub struct Actor<T>
where
    T: xtra::Actor,
{
    monitor: Monitor,
    cfds: HashMap<OrderId, MonitorParams>,
    cfd_actor_addr: xtra::Address<T>,
}

impl<T> xtra::Actor for Actor<T> where T: xtra::Actor {}

// TODO: The traitbound for LockFinality should not be needed here, but we could not work around it
#[async_trait]
impl<T> xtra::Handler<StartMonitoring> for Actor<T>
where
    T: xtra::Actor + xtra::Handler<CfdMonitoringEvent>,
{
    async fn handle(&mut self, msg: StartMonitoring, _ctx: &mut xtra::Context<Self>) {
        log_error!(self.handle_start_monitoring(msg));
    }
}
