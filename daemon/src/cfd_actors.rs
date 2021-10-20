use crate::db::load_cfd_by_order_id;
use crate::model::cfd::{Attestation, Cfd, CfdState, CfdStateChangeEvent, OrderId};
use crate::wallet::Wallet;
use crate::{db, monitor, oracle, try_continue};
use anyhow::{bail, Context, Result};
use sqlx::pool::PoolConnection;
use sqlx::Sqlite;
use tokio::sync::watch;

pub async fn insert_cfd(
    cfd: &Cfd,
    conn: &mut PoolConnection<Sqlite>,
    update_sender: &watch::Sender<Vec<Cfd>>,
) -> Result<()> {
    if load_cfd_by_order_id(cfd.order.id, conn).await.is_ok() {
        bail!(
            "Cannot insert cfd because there is already a cfd for order id {}",
            cfd.order.id
        )
    }

    db::insert_cfd(cfd, conn).await?;
    update_sender.send(db::load_all_cfds(conn).await?)?;
    Ok(())
}

pub async fn append_cfd_state(
    cfd: &Cfd,
    conn: &mut PoolConnection<Sqlite>,
    update_sender: &watch::Sender<Vec<Cfd>>,
) -> Result<()> {
    db::append_cfd_state(cfd, conn).await?;
    update_sender.send(db::load_all_cfds(conn).await?)?;
    Ok(())
}

pub async fn try_cet_publication(
    cfd: &mut Cfd,
    conn: &mut PoolConnection<Sqlite>,
    wallet: &Wallet,
    update_sender: &watch::Sender<Vec<Cfd>>,
) -> Result<()> {
    match cfd.cet()? {
        Ok(cet) => {
            let txid = wallet.try_broadcast_transaction(cet).await?;
            tracing::info!("CET published with txid {}", txid);

            if cfd.handle(CfdStateChangeEvent::CetSent)?.is_none() {
                bail!("If we can get the CET we should be able to transition")
            }

            append_cfd_state(cfd, conn, update_sender).await?;
        }
        Err(not_ready_yet) => {
            tracing::debug!("{:#}", not_ready_yet);
            return Ok(());
        }
    };

    Ok(())
}

pub async fn handle_monitoring_event(
    event: monitor::Event,
    conn: &mut PoolConnection<Sqlite>,
    wallet: &Wallet,
    update_sender: &watch::Sender<Vec<Cfd>>,
) -> Result<()> {
    let order_id = event.order_id();

    let mut cfd = db::load_cfd_by_order_id(order_id, conn).await?;

    if cfd.handle(CfdStateChangeEvent::Monitor(event))?.is_none() {
        // early exit if there was not state change
        // this is for cases where we are already in a final state
        return Ok(());
    }

    append_cfd_state(&cfd, conn, update_sender).await?;

    if let CfdState::OpenCommitted { .. } = cfd.state {
        try_cet_publication(&mut cfd, conn, wallet, update_sender).await?;
    } else if let CfdState::MustRefund { .. } = cfd.state {
        let signed_refund_tx = cfd.refund_tx()?;
        let txid = wallet.try_broadcast_transaction(signed_refund_tx).await?;

        tracing::info!("Refund transaction published on chain: {}", txid);
    }
    Ok(())
}

pub async fn handle_commit(
    order_id: OrderId,
    conn: &mut PoolConnection<Sqlite>,
    wallet: &Wallet,
    update_sender: &watch::Sender<Vec<Cfd>>,
) -> Result<()> {
    let mut cfd = db::load_cfd_by_order_id(order_id, conn).await?;

    let signed_commit_tx = cfd.commit_tx()?;

    let txid = wallet.try_broadcast_transaction(signed_commit_tx).await?;

    if cfd.handle(CfdStateChangeEvent::CommitTxSent)?.is_none() {
        bail!("If we can get the commit tx we should be able to transition")
    }

    append_cfd_state(&cfd, conn, update_sender).await?;
    tracing::info!("Commit transaction published on chain: {}", txid);

    Ok(())
}

pub async fn handle_oracle_attestation(
    attestation: oracle::Attestation,
    conn: &mut PoolConnection<Sqlite>,
    wallet: &Wallet,
    update_sender: &watch::Sender<Vec<Cfd>>,
) -> Result<()> {
    tracing::debug!(
        "Learnt latest oracle attestation for event: {}",
        attestation.id
    );

    let mut cfds = db::load_cfds_by_oracle_event_id(attestation.id, conn).await?;

    for (cfd, dlc) in cfds
        .iter_mut()
        .filter_map(|cfd| cfd.dlc().map(|dlc| (cfd, dlc)))
    {
        let attestation = try_continue!(Attestation::new(
            attestation.id,
            attestation.price,
            attestation.scalars.clone(),
            dlc,
            cfd.role(),
        ));

        let new_state =
            try_continue!(cfd.handle(CfdStateChangeEvent::OracleAttestation(attestation)));

        if new_state.is_none() {
            // if we don't transition to a new state after oracle attestation we ignore the cfd
            // this is for cases where we cannot handle the attestation which should be in a
            // final state
            continue;
        }

        try_continue!(append_cfd_state(cfd, conn, update_sender).await);
        try_continue!(try_cet_publication(cfd, conn, wallet, update_sender)
            .await
            .context("Error when trying to publish CET"));
    }

    Ok(())
}
