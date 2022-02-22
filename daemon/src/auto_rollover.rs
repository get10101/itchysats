use crate::cfd_actors::load_cfd;
use crate::connection;
use crate::db;
use crate::oracle;
use crate::process_manager;
use crate::rollover_taker;
use crate::try_continue;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use maia::secp256k1_zkp::schnorrsig;
use model::OrderId;
use std::time::Duration;
use time::OffsetDateTime;
use tokio_tasks::Tasks;
use xtra::Actor as _;
use xtra::Address;
use xtra_productivity::xtra_productivity;
use xtras::address_map::Stopping;
use xtras::AddressMap;
use xtras::SendAsyncSafe;
use xtras::SendInterval;

pub struct Actor<O> {
    db: sqlx::SqlitePool,
    oracle_pk: schnorrsig::PublicKey,
    process_manager: Address<process_manager::Actor>,
    conn: Address<connection::Actor>,
    oracle: Address<O>,
    n_payouts: usize,
    rollover_actors: AddressMap<OrderId, rollover_taker::Actor>,
    tasks: Tasks,
}

impl<O> Actor<O> {
    pub fn new(
        db: sqlx::SqlitePool,
        oracle_pk: schnorrsig::PublicKey,
        process_manager: Address<process_manager::Actor>,
        conn: Address<connection::Actor>,
        oracle: Address<O>,
        n_payouts: usize,
    ) -> Self {
        Self {
            db,
            oracle_pk,
            process_manager,
            conn,
            oracle,
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
    async fn handle(&mut self, _msg: AutoRollover, ctx: &mut xtra::Context<Self>) {
        tracing::trace!("Checking all CFDs for rollover eligibility");

        // Auto-rollover is invoked periodically by `addr.send_interval()`,
        // which does not handle errors - forward implementation to allow `?` inside
        if let Err(e) = self.handle_auto_rollover_impl(ctx).await {
            tracing::error!("Auto-rollover failed: {:#}", e);
        }
    }

    async fn handle(&mut self, Rollover(order_id): Rollover, ctx: &mut xtra::Context<Self>) {
        let disconnected = match self.rollover_actors.get_disconnected(order_id) {
            Ok(disconnected) => disconnected,
            Err(_) => {
                tracing::debug!(%order_id, "Rollover already in progress");
                return;
            }
        };

        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");
        let (addr, fut) = rollover_taker::Actor::new(
            order_id,
            self.n_payouts,
            self.oracle_pk,
            self.conn.clone(),
            &self.oracle,
            self.process_manager.clone(),
            (&this, &self.conn),
            self.db.clone(),
        )
        .create(None)
        .run();

        disconnected.insert(addr);
        self.tasks.add(fut);
    }
}

impl<O> Actor<O>
where
    O: xtra::Handler<oracle::GetAnnouncement>,
{
    async fn handle_auto_rollover_impl(
        &mut self,
        ctx: &mut xtra::Context<Actor<O>>,
    ) -> Result<(), anyhow::Error> {
        let mut conn = self.db.acquire().await?;
        let cfd_ids = db::load_open_cfd_ids(&mut conn).await?;
        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");
        for id in cfd_ids {
            try_continue!(async {
                let cfd = load_cfd(id, &mut conn).await?;
                match cfd.can_auto_rollover_taker(OffsetDateTime::now_utc()) {
                    Ok(()) => this.send_async_safe(Rollover(id)).await?,
                    Err(reason) => {
                        tracing::trace!(order_id = %id, %reason, "CFD is not eligible for auto-rollover");
                    }
                }
                anyhow::Ok(())
            }
            .await
            .context("Cannot roll over"));
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
    O: xtra::Handler<oracle::GetAnnouncement> + 'static,
{
    type Stop = ();

    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");
        self.tasks
            .add(this.send_interval(Duration::from_secs(5 * 60), || AutoRollover));
    }

    async fn stopped(self) -> Self::Stop {}
}

/// Message sent to ourselves at an interval to check if rollover can
/// be triggered for any of the CFDs in the database.
#[derive(Clone, Copy)]
pub struct AutoRollover;

/// Message used to trigger rollover internally within the `auto_rollover::Actor`
///
/// This helps us trigger rollover in the tests unconditionally of time.
#[derive(Clone, Copy)]
pub struct Rollover(pub OrderId);
