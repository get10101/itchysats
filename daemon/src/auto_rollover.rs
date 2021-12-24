use crate::address_map::AddressMap;
use crate::address_map::Stopping;
use crate::cfd_actors::load_cfd;
use crate::connection;
use crate::db;
use crate::model::cfd::OrderId;
use crate::model::cfd::RolloverCompleted;
use crate::oracle;
use crate::process_manager;
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

pub struct Actor<O> {
    db: sqlx::SqlitePool,
    oracle_pk: schnorrsig::PublicKey,
    process_manager: Address<process_manager::Actor>,
    projection_actor: Address<projection::Actor>,
    conn_actor: Address<connection::Actor>,
    oracle_actor: Address<O>,
    n_payouts: usize,

    rollover_actors: AddressMap<OrderId, rollover_taker::Actor>,

    tasks: Tasks,
}

impl<O> Actor<O> {
    pub fn new(
        db: sqlx::SqlitePool,
        oracle_pk: schnorrsig::PublicKey,
        process_manager: Address<process_manager::Actor>,
        projection_actor: Address<projection::Actor>,
        conn_actor: Address<connection::Actor>,
        oracle_actor: Address<O>,
        n_payouts: usize,
    ) -> Self {
        Self {
            db,
            oracle_pk,
            process_manager,
            projection_actor,
            conn_actor,
            oracle_actor,
            n_payouts,
            rollover_actors: AddressMap::default(),
            tasks: Tasks::default(),
        }
    }
}

#[xtra_productivity]
impl<O> Actor<O>
where
    O: xtra::Handler<oracle::MonitorAttestation> + xtra::Handler<oracle::GetAnnouncement>,
{
    async fn handle(&mut self, _msg: AutoRollover, ctx: &mut xtra::Context<Self>) -> Result<()> {
        tracing::trace!("Checking all CFDs for rollover eligibility");

        let mut conn = self.db.acquire().await?;
        let cfd_ids = db::load_all_cfd_ids(&mut conn).await?;

        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");

        for id in cfd_ids {
            let disconnected = match self.rollover_actors.get_disconnected(id) {
                Ok(disconnected) => disconnected,
                Err(_) => {
                    tracing::debug!(order_id=%id, "Rollover already in progress");
                    continue;
                }
            };

            // TODO: Shall this have a try_continue?
            let cfd = load_cfd(id, &mut conn).await?;

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

#[xtra_productivity(message_impl = false)]
impl<O> Actor<O>
where
    O: 'static + xtra::Handler<oracle::MonitorAttestation> + xtra::Handler<oracle::GetAnnouncement>,
{
    async fn handle_rollover_completed(&mut self, msg: RolloverCompleted) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        let order_id = msg.order_id();

        let cfd = load_cfd(order_id, &mut conn).await?;

        let event = match cfd.rollover(msg)? {
            Some(event) => event,
            None => return Ok(()),
        };

        match self
            .process_manager
            .send(process_manager::Event::new(event.clone()))
            .await?
        {
            Ok(()) => {
                tracing::info!("Rollover completed");
            }
            Err(e) => {
                tracing::warn!("Failed to process {:?}: {:#}", event, e);
            }
        }

        Ok(())
    }
}

#[xtra_productivity(message_impl = false)]
impl<O> Actor<O>
where
    O: xtra::Handler<oracle::MonitorAttestation> + xtra::Handler<oracle::GetAnnouncement>,
{
    async fn handle_rollover_actor_stopping(&mut self, msg: Stopping<rollover_taker::Actor>) {
        self.rollover_actors.gc(msg);
    }
}

#[async_trait]
impl<O> xtra::Actor for Actor<O>
where
    O: 'static,
    Self: xtra::Handler<AutoRollover>,
{
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let fut = ctx
            .notify_interval(Duration::from_secs(5 * 60), || AutoRollover)
            .expect("we are alive");

        self.tasks.add(fut);
    }
}

/// Message to trigger roll-over on a regular interval
pub struct AutoRollover;
