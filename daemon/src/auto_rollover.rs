use crate::address_map::AddressMap;
use crate::address_map::Stopping;
use crate::cfd_actors::load_cfd;
use crate::connection;
use crate::db;
use crate::db::append_event;
use crate::model::cfd::OrderId;
use crate::model::cfd::RolloverCompleted;
use crate::monitor;
use crate::oracle;
use crate::process_manager;
use crate::projection;
use crate::rollover_taker;
use crate::send_async_safe::SendAsyncSafe;
use crate::try_continue;
use crate::Tasks;
use anyhow::Result;
use async_trait::async_trait;
use maia::secp256k1_zkp::schnorrsig;
use std::time::Duration;
use time::OffsetDateTime;
use xtra::Actor as _;
use xtra::Address;
use xtra_productivity::xtra_productivity;

pub struct Actor<O> {
    db: sqlx::SqlitePool,
    oracle_pk: schnorrsig::PublicKey,
    projection_actor: Address<projection::Actor>,
    process_manager_actor: Address<process_manager::Actor>,
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
        projection_actor: Address<projection::Actor>,
        process_manager_actor: Address<process_manager::Actor>,
        conn_actor: Address<connection::Actor>,
        oracle_actor: Address<O>,
        n_payouts: usize,
    ) -> Self {
        Self {
            db,
            oracle_pk,
            projection_actor,
            process_manager_actor,
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
    O: xtra::Handler<oracle::GetAnnouncement>,
{
    async fn handle(&mut self, _msg: AutoRollover, ctx: &mut xtra::Context<Self>) -> Result<()> {
        tracing::trace!("Checking all CFDs for rollover eligibility");

        let mut conn = self.db.acquire().await?;
        let cfd_ids = db::load_all_cfd_ids(&mut conn).await?;

        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");

        for id in cfd_ids {
            let cfd = try_continue!(load_cfd(id, &mut conn).await);

            if let Err(e) = cfd.can_auto_rollover_taker(OffsetDateTime::now_utc()) {
                tracing::trace!(%id, "Cannot roll over: {:#}", e);
                continue;
            }

            this.send_async_safe(Rollover { id }).await?;
        }

        Ok(())
    }

    async fn handle(&mut self, msg: Rollover, ctx: &mut xtra::Context<Self>) -> Result<()> {
        let id = msg.id;

        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd(id, &mut conn).await?;

        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");

        let disconnected = match self.rollover_actors.get_disconnected(id) {
            Ok(disconnected) => disconnected,
            Err(_) => {
                tracing::debug!(order_id=%id, "Rollover already in progress");
                return Ok(());
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

        Ok(())
    }
}

#[xtra_productivity(message_impl = false)]
impl<O> Actor<O>
where
    O: 'static,
    O: xtra::Handler<oracle::GetAnnouncement>,
{
    async fn handle_rollover_completed(
        &mut self,
        rollover_completed: RolloverCompleted,
    ) -> Result<()> {
        let id = rollover_completed.order_id();

        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd(id, &mut conn).await?;

        let event = cfd.roll_over(rollover_completed)?;
        append_event(event.clone(), &mut conn).await?;

        if let Err(e) = self
            .process_manager_actor
            .send(process_manager::Event::new(event.clone()))
            .await?
        {
            tracing::error!("Sending event to process manager failed: {:#}", e);
        }

        Ok(())
    }
}

#[xtra_productivity(message_impl = false)]
impl<O> Actor<O>
where
    O: xtra::Handler<oracle::GetAnnouncement>,
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
            .notify_interval(
                rollover_taker::MAX_ROLLOVER_DURATION + Duration::from_secs(60),
                || AutoRollover,
            )
            .expect("we are alive");

        self.tasks.add(fut);
    }
}

/// Message to trigger auto-rollover on a regular interval
pub struct AutoRollover;

/// Message used to trigger rollover internally within the `auto_rollover::Actor`
///
/// This helps us trigger rollover in the tests unconditionally of time.
pub struct Rollover {
    id: OrderId,
}
