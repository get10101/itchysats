use crate::address_map::AddressMap;
use crate::address_map::Stopping;
use crate::cfd_actors::append_cfd_state;
use crate::connection;
use crate::db;
use crate::db::load_cfd;
use crate::model::cfd::CannotRollover;
use crate::model::cfd::CfdState;
use crate::model::cfd::CfdStateCommon;
use crate::model::cfd::OrderId;
use crate::model::cfd::RolloverCompleted;
use crate::monitor;
use crate::monitor::MonitorParams;
use crate::oracle;
use crate::projection;
use crate::rollover_taker;
use crate::Tasks;
use anyhow::Result;
use async_trait::async_trait;
use maia::secp256k1_zkp::schnorrsig;
use std::time::Duration;
use xtra::Actor as _;
use xtra::Address;
use xtra_productivity::xtra_productivity;

pub struct Actor<O, M> {
    db: sqlx::SqlitePool,
    oracle_pk: schnorrsig::PublicKey,
    projection_actor: Address<projection::Actor>,
    conn_actor: Address<connection::Actor>,
    monitor_actor: Address<M>,
    oracle_actor: Address<O>,
    n_payouts: usize,

    rollover_actors: AddressMap<OrderId, rollover_taker::Actor>,

    tasks: Tasks,
}

impl<O, M> Actor<O, M> {
    pub fn new(
        db: sqlx::SqlitePool,
        oracle_pk: schnorrsig::PublicKey,
        projection_actor: Address<projection::Actor>,
        conn_actor: Address<connection::Actor>,
        monitor_actor: Address<M>,
        oracle_actor: Address<O>,
        n_payouts: usize,
    ) -> Self {
        Self {
            db,
            oracle_pk,
            projection_actor,
            conn_actor,
            monitor_actor,
            oracle_actor,
            n_payouts,
            rollover_actors: AddressMap::default(),
            tasks: Tasks::default(),
        }
    }
}

#[xtra_productivity]
impl<O, M> Actor<O, M>
where
    M: xtra::Handler<monitor::StartMonitoring>,
    O: xtra::Handler<oracle::MonitorAttestation> + xtra::Handler<oracle::GetAnnouncement>,
{
    async fn handle(&mut self, _msg: AutoRollover, ctx: &mut xtra::Context<Self>) -> Result<()> {
        tracing::trace!("Checking all CFDs for rollover eligibility");

        let mut conn = self.db.acquire().await?;
        let cfds = db::load_all_cfds(&mut conn).await?;

        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");

        for cfd in cfds {
            let disconnected = match self.rollover_actors.get_disconnected(cfd.id()) {
                Ok(disconnected) => disconnected,
                Err(_) => {
                    tracing::debug!(order_id=%cfd.id(), "Rollover already in progress");
                    continue;
                }
            };

            let (addr, fut) = rollover_taker::Actor::new(
                (cfd, self.n_payouts),
                self.oracle_pk,
                self.conn_actor.clone(),
                &self.oracle_actor,
                self.projection_actor.clone(),
                &this,
                (&this, &self.conn_actor),
            )
            .create(None)
            .run();

            disconnected.insert(addr);
            self.tasks.add(fut);
        }

        Ok(())
    }
}

#[xtra_productivity]
impl<O, M> Actor<O, M>
where
    O: 'static,
    M: 'static,
    M: xtra::Handler<monitor::StartMonitoring>,
    O: xtra::Handler<oracle::MonitorAttestation> + xtra::Handler<oracle::GetAnnouncement>,
{
    async fn handle_rollover_completed(&mut self, msg: RolloverCompleted) -> Result<()> {
        let (order_id, dlc) = match msg {
            RolloverCompleted::Succeeded {
                order_id,
                payload: (dlc, _),
            } => (order_id, dlc),
            RolloverCompleted::Rejected { order_id, reason } => {
                tracing::debug!(%order_id, "Not rolled over: {:#}", reason);
                return Ok(());
            }
            RolloverCompleted::Failed { order_id, error } => {
                tracing::warn!(%order_id, "Rollover failed: {:#}", error);
                return Ok(());
            }
        };

        let mut conn = self.db.acquire().await?;
        let mut cfd = load_cfd(order_id, &mut conn).await?;
        *cfd.state_mut() = CfdState::Open {
            common: CfdStateCommon::default(),
            dlc: dlc.clone(),
            attestation: None,
            collaborative_close: None,
        };

        append_cfd_state(&cfd, &mut conn, &self.projection_actor).await?;

        self.monitor_actor
            .send(monitor::StartMonitoring {
                id: order_id,
                params: MonitorParams::new(dlc.clone(), cfd.refund_timelock_in_blocks()),
            })
            .await?;

        self.oracle_actor
            .send(oracle::MonitorAttestation {
                event_id: dlc.settlement_event_id,
            })
            .await?;

        Ok(())
    }
}

impl<O, M> Actor<O, M>
where
    O: 'static,
    M: 'static,
    M: xtra::Handler<monitor::StartMonitoring>,
    O: xtra::Handler<oracle::MonitorAttestation> + xtra::Handler<oracle::GetAnnouncement>,
{
    async fn handle_rollover_actor_stopping(&mut self, msg: Stopping<rollover_taker::Actor>) {
        self.rollover_actors.gc(msg);
    }
}

#[async_trait]
impl<O, M> xtra::Actor for Actor<O, M>
where
    O: 'static,
    M: 'static,
    Self: xtra::Handler<AutoRollover>,
{
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let fut = ctx
            .notify_interval(
                rollover_taker::MAX_ROLLOVER_DURATION + Duration::from_secs(60),
                || AutoRollover,
            )
            .expect("we are alive");

        self.tasks.add(fut);
    }
}

/// Message to trigger roll-over on a regular interval
pub struct AutoRollover;
