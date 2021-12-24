use crate::db;
use crate::model::cfd::Cfd;
use crate::model::cfd::CfdEvent;
use crate::model::cfd::Event;
use crate::model::cfd::OrderId;
use crate::monitor;
use crate::oracle;
use crate::process_manager;
use crate::projection;
use crate::try_continue;
use crate::wallet;
use anyhow::Context;
use anyhow::Result;
use sqlx::pool::PoolConnection;
use sqlx::Sqlite;
use sqlx::SqlitePool;

pub async fn insert_cfd_and_update_feed(
    cfd: &Cfd,
    conn: &mut PoolConnection<Sqlite>,
    projection_address: &xtra::Address<projection::Actor>,
) -> Result<()> {
    db::insert_cfd(cfd, conn).await?;
    projection_address.send(projection::CfdsChanged).await?;
    Ok(())
}

pub async fn handle_monitoring_event<W>(
    event: monitor::Event,
    db: &SqlitePool,
    wallet: &xtra::Address<W>,
    process_manager_address: &xtra::Address<process_manager::Actor>,
) -> Result<()>
where
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    let mut conn = db.acquire().await?;

    let order_id = event.order_id();

    let cfd = load_cfd(order_id, &mut conn).await?;

    let event = match event {
        monitor::Event::LockFinality(_) => cfd.handle_lock_confirmed(),
        monitor::Event::CommitFinality(_) => cfd.handle_commit_confirmed(),
        monitor::Event::CloseFinality(_) => cfd.handle_collaborative_settlement_confirmed(),
        monitor::Event::CetTimelockExpired(_) => {
            if let Ok(event) = cfd.handle_cet_timelock_expired() {
                event
            } else {
                return Ok(()); // Early return from a no-op
            }
        }
        monitor::Event::CetFinality(_) => cfd.handle_cet_confirmed(),
        monitor::Event::RefundTimelockExpired(_) => cfd.handle_refund_timelock_expired(),
        monitor::Event::RefundFinality(_) => cfd.handle_refund_confirmed(),
        monitor::Event::RevokedTransactionFound(_) => cfd.handle_revoke_confirmed(),
    };

    if let Err(e) = process_manager_address
        .send(process_manager::Event::new(event.clone()))
        .await?
    {
        tracing::error!("Sending event to process manager failed: {:#}", e);
    } else {
        // TODO: Move into process manager
        post_process_event(event, wallet).await?;
    }

    Ok(())
}

/// Load a CFD from the database and rehydrate as the [`model::cfd::Cfd`] aggregate.
pub async fn load_cfd(order_id: OrderId, conn: &mut PoolConnection<Sqlite>) -> Result<Cfd> {
    let (
        db::Cfd {
            id,
            position,
            initial_price,
            leverage,
            settlement_interval,
            counterparty_network_identity,
            role,
            quantity_usd,
        },
        events,
    ) = db::load_cfd(order_id, conn).await?;
    let cfd = Cfd::rehydrate(
        id,
        position,
        initial_price,
        leverage,
        settlement_interval,
        quantity_usd,
        counterparty_network_identity,
        role,
        events,
    );
    Ok(cfd)
}

pub async fn handle_commit<W>(
    order_id: OrderId,
    conn: &mut PoolConnection<Sqlite>,
    wallet: &xtra::Address<W>,
    process_manager_address: &xtra::Address<process_manager::Actor>,
) -> Result<()>
where
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    let cfd = load_cfd(order_id, conn).await?;

    let event = cfd.manual_commit_to_blockchain()?;
    if let Err(e) = process_manager_address
        .send(process_manager::Event::new(event.clone()))
        .await?
    {
        tracing::error!("Sending event to process manager failed: {:#}", e);
    } else {
        // TODO: Move into process manager
        post_process_event(event, wallet).await?;
    }

    Ok(())
}

pub async fn handle_oracle_attestation<W>(
    attestation: oracle::Attestation,
    db: &SqlitePool,
    wallet: &xtra::Address<W>,
    process_manager_address: &xtra::Address<process_manager::Actor>,
) -> Result<()>
where
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    let mut conn = db.acquire().await?;

    tracing::debug!(
        "Learnt latest oracle attestation for event: {}",
        attestation.id
    );

    for id in db::load_all_cfd_ids(&mut conn).await? {
        let cfd = try_continue!(load_cfd(id, &mut conn).await);
        let event = try_continue!(cfd
            .decrypt_cet(&attestation)
            .context("Failed to decrypt CET using attestation"));

        if let Some(event) = event {
            // Note: ? OK, because if the actor is disconnected we can fail the loop
            if let Err(e) = process_manager_address
                .send(process_manager::Event::new(event.clone()))
                .await?
            {
                tracing::error!("Sending event to process manager failed: {:#}", e);
            } else {
                // TODO: Move into process manager
                try_continue!(post_process_event(event, wallet).await);
            }
        }
    }

    Ok(())
}

async fn post_process_event<W>(event: Event, wallet: &xtra::Address<W>) -> Result<()>
where
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    match event.event {
        CfdEvent::OracleAttestedPostCetTimelock { cet, .. }
        | CfdEvent::CetTimelockConfirmedPostOracleAttestation { cet } => {
            let txid = wallet
                .send(wallet::TryBroadcastTransaction { tx: cet })
                .await?
                .context("Failed to broadcast CET")?;

            tracing::info!(%txid, "CET published");
        }
        CfdEvent::OracleAttestedPriorCetTimelock { commit_tx: tx, .. }
        | CfdEvent::ManualCommit { tx } => {
            let txid = wallet
                .send(wallet::TryBroadcastTransaction { tx })
                .await?
                .context("Failed to broadcast commit transaction")?;

            tracing::info!(%txid, "Commit transaction published");
        }

        _ => {}
    }

    Ok(())
}
