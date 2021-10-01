use crate::actors::log_error;
use crate::model::cfd::{CetStatus, Cfd, CfdState, Dlc, OrderId};
use crate::monitor::subscription::Subscription;
use crate::oracle;
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
    lock: (Txid, Descriptor<PublicKey>),
    commit: (Txid, Descriptor<PublicKey>),
    cets: Vec<(Txid, Script, RangeInclusive<u64>)>,
    refund: (Txid, Script, u32),
}

impl MonitorParams {
    pub fn from_dlc_and_timelocks(dlc: Dlc, refund_timelock_in_blocks: u32) -> Self {
        let script_pubkey = dlc.address.script_pubkey();
        MonitorParams {
            lock: (dlc.lock.0.txid(), dlc.lock.1),
            commit: (dlc.commit.0.txid(), dlc.commit.2),
            cets: dlc
                .cets
                .into_iter()
                .map(|(tx, _, range)| (tx.txid(), script_pubkey.clone(), range))
                .collect(),
            refund: (
                dlc.refund.0.txid(),
                script_pubkey,
                refund_timelock_in_blocks,
            ),
        }
    }
}

impl<T> Actor<T>
where
    T: xtra::Actor + xtra::Handler<Event>,
{
    pub async fn new(
        electrum_rpc_url: &str,
        cfd_actor_addr: xtra::Address<T>,
        cfds: Vec<Cfd>,
    ) -> Self {
        let monitor = Monitor::new(electrum_rpc_url, FINALITY_CONFIRMATIONS).unwrap();

        let mut actor = Self {
            monitor,
            cfds: HashMap::new(),
            cfd_actor_addr,
        };

        for cfd in cfds {
            match cfd.state.clone() {
                // In PendingOpen we know the complete dlc setup and assume that the lock transaction will be published
                CfdState::PendingOpen { dlc, .. } => {
                    let params = MonitorParams::from_dlc_and_timelocks(dlc.clone(), cfd.refund_timelock_in_blocks());
                    actor.cfds.insert(cfd.order.id, params.clone());
                    actor.monitor_all(&params, cfd.order.id).await;
                }
                CfdState::Open { dlc, .. } => {
                    let params = MonitorParams::from_dlc_and_timelocks(dlc.clone(), cfd.refund_timelock_in_blocks());
                    actor.cfds.insert(cfd.order.id, params.clone());

                    actor.monitor_commit_finality_and_timelocks(&params, cfd.order.id).await;
                    actor.monitor_refund_finality( params.clone(),cfd.order.id).await;
                }
                CfdState::OpenCommitted { dlc, cet_status, .. } => {
                    let params = MonitorParams::from_dlc_and_timelocks(dlc.clone(), cfd.refund_timelock_in_blocks());
                    actor.cfds.insert(cfd.order.id, params.clone());

                    let commit_subscription = actor
                        .monitor
                        .subscribe_to((params.commit.0, params.commit.1.script_pubkey()))
                        .await;

                    match cet_status {
                        CetStatus::Unprepared
                        | CetStatus::OracleSigned(_) => {
                            actor.monitor_commit_cet_timelock(cfd.order.id, &commit_subscription).await;
                            actor.monitor_commit_refund_timelock(&params, cfd.order.id, &commit_subscription).await;
                            actor.monitor_refund_finality( params.clone(),cfd.order.id).await;
                        }
                        CetStatus::TimelockExpired => {
                            actor.monitor_commit_refund_timelock(&params, cfd.order.id, &commit_subscription).await;
                            actor.monitor_refund_finality( params.clone(),cfd.order.id).await;
                        }
                        CetStatus::Ready(_price) => {
                            // TODO: monitor CET finality

                            actor.monitor_commit_refund_timelock(&params, cfd.order.id, &commit_subscription).await;
                            actor.monitor_refund_finality( params.clone(),cfd.order.id).await;
                        }
                    }
                }
                CfdState::MustRefund { dlc, .. } => {
                    // TODO: CET monitoring (?) - note: would require to add CetStatus information to MustRefund

                    let params = MonitorParams::from_dlc_and_timelocks(dlc.clone(), cfd.refund_timelock_in_blocks());
                    actor.cfds.insert(cfd.order.id, params.clone());

                    let commit_subscription = actor
                        .monitor
                        .subscribe_to((params.commit.0, params.commit.1.script_pubkey()))
                        .await;

                    actor.monitor_commit_refund_timelock(&params, cfd.order.id, &commit_subscription).await;
                    actor.monitor_refund_finality( params.clone(),cfd.order.id).await;
                }

                // too early to monitor
                CfdState::OutgoingOrderRequest { .. }
                | CfdState::IncomingOrderRequest { .. }
                | CfdState::Accepted { .. }
                | CfdState::ContractSetup { .. }

                // final states
                | CfdState::Rejected { .. }
                | CfdState::Refunded { .. }
                | CfdState::SetupFailed { .. } => ()
            }
        }

        actor
    }

    async fn handle_start_monitoring(&mut self, msg: StartMonitoring) -> Result<()> {
        let StartMonitoring { id, params } = msg;

        self.cfds.insert(id, params.clone());
        self.monitor_all(&params, id).await;

        Ok(())
    }

    async fn monitor_all(&self, params: &MonitorParams, order_id: OrderId) {
        self.monitor_lock_finality(params, order_id).await;

        self.monitor_commit_finality_and_timelocks(params, order_id)
            .await;

        self.monitor_refund_finality(params.clone(), order_id).await;
    }

    async fn monitor_lock_finality(&self, params: &MonitorParams, order_id: OrderId) {
        tokio::spawn({
            let cfd_actor_addr = self.cfd_actor_addr.clone();
            let lock_subscription = self
                .monitor
                .subscribe_to((params.lock.0, params.lock.1.script_pubkey()))
                .await;
            async move {
                lock_subscription.wait_until_final().await.unwrap();

                cfd_actor_addr
                    .do_send_async(Event::LockFinality(order_id))
                    .await
                    .unwrap();
            }
        });
    }

    async fn monitor_commit_finality_and_timelocks(
        &self,
        params: &MonitorParams,
        order_id: OrderId,
    ) {
        let commit_subscription = self
            .monitor
            .subscribe_to((params.commit.0, params.commit.1.script_pubkey()))
            .await;

        self.monitor_commit_finality(order_id, &commit_subscription)
            .await;
        self.monitor_commit_cet_timelock(order_id, &commit_subscription)
            .await;
        self.monitor_commit_refund_timelock(params, order_id, &commit_subscription)
            .await;
    }

    async fn monitor_commit_finality(&self, order_id: OrderId, commit_subscription: &Subscription) {
        tokio::spawn({
            let cfd_actor_addr = self.cfd_actor_addr.clone();
            let commit_subscription = commit_subscription.clone();
            async move {
                commit_subscription.wait_until_final().await.unwrap();

                cfd_actor_addr
                    .do_send_async(Event::CommitFinality(order_id))
                    .await
                    .unwrap();
            }
        });
    }

    async fn monitor_commit_cet_timelock(
        &self,
        order_id: OrderId,
        commit_subscription: &Subscription,
    ) {
        tokio::spawn({
            let cfd_actor_addr = self.cfd_actor_addr.clone();
            let commit_subscription = commit_subscription.clone();
            async move {
                commit_subscription
                    .wait_until_confirmed_with(Cfd::CET_TIMELOCK)
                    .await
                    .unwrap();

                cfd_actor_addr
                    .do_send_async(Event::CetTimelockExpired(order_id))
                    .await
                    .unwrap();
            }
        });
    }

    async fn monitor_commit_refund_timelock(
        &self,
        params: &MonitorParams,
        order_id: OrderId,
        commit_subscription: &Subscription,
    ) {
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
                    .do_send_async(Event::RefundTimelockExpired(order_id))
                    .await
                    .unwrap();
            }
        });
    }

    async fn monitor_refund_finality(&self, params: MonitorParams, order_id: OrderId) {
        tokio::spawn({
            let cfd_actor_addr = self.cfd_actor_addr.clone();
            let refund_subscription = self
                .monitor
                .subscribe_to((params.refund.0, params.refund.1))
                .await;
            async move {
                refund_subscription.wait_until_final().await.unwrap();

                cfd_actor_addr
                    .do_send_async(Event::RefundFinality(order_id))
                    .await
                    .unwrap();
            }
        });
    }

    pub async fn handle_oracle_attestation(&self, attestation: oracle::Attestation) -> Result<()> {
        for (order_id, MonitorParams { cets, .. }) in self.cfds.clone().into_iter() {
            let (txid, script_pubkey) =
                match cets.iter().find_map(|(txid, script_pubkey, range)| {
                    range
                        .contains(&attestation.price)
                        .then(|| (txid, script_pubkey))
                }) {
                    Some(cet) => cet,
                    None => continue,
                };

            tokio::spawn({
                let cfd_actor_addr = self.cfd_actor_addr.clone();
                let cet_subscription = self
                    .monitor
                    .subscribe_to((*txid, script_pubkey.clone()))
                    .await;
                async move {
                    cet_subscription.wait_until_final().await.unwrap();

                    cfd_actor_addr
                        .do_send_async(Event::CetFinality(order_id))
                        .await
                        .unwrap();
                }
            });
        }

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
pub enum Event {
    LockFinality(OrderId),
    CommitFinality(OrderId),
    CetTimelockExpired(OrderId),
    CetFinality(OrderId),
    RefundTimelockExpired(OrderId),
    RefundFinality(OrderId),
}

impl Event {
    pub fn order_id(&self) -> OrderId {
        let order_id = match self {
            Event::LockFinality(order_id) => order_id,
            Event::CommitFinality(order_id) => order_id,
            Event::CetTimelockExpired(order_id) => order_id,
            Event::RefundTimelockExpired(order_id) => order_id,
            Event::RefundFinality(order_id) => order_id,
            Event::CetFinality(order_id) => order_id,
        };

        *order_id
    }
}

impl xtra::Message for Event {
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

#[async_trait]
impl<T> xtra::Handler<StartMonitoring> for Actor<T>
where
    T: xtra::Actor + xtra::Handler<Event>,
{
    async fn handle(&mut self, msg: StartMonitoring, _ctx: &mut xtra::Context<Self>) {
        log_error!(self.handle_start_monitoring(msg));
    }
}

#[async_trait]
impl<T> xtra::Handler<oracle::Attestation> for Actor<T>
where
    T: xtra::Actor + xtra::Handler<Event>,
{
    async fn handle(&mut self, msg: oracle::Attestation, _ctx: &mut xtra::Context<Self>) {
        log_error!(self.handle_oracle_attestation(msg));
    }
}
