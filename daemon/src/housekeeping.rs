use crate::db::{append_cfd_state, load_all_cfds};
use crate::model::cfd::{Cfd, CfdState};
use crate::wallet::Wallet;
use anyhow::Result;
use bdk::bitcoin::Transaction;
use sqlx::pool::PoolConnection;
use sqlx::Sqlite;

pub async fn transition_non_continue_cfds_to_setup_failed(
    conn: &mut PoolConnection<Sqlite>,
) -> Result<()> {
    let mut cfds = load_all_cfds(conn).await?;

    for cfd in cfds.iter_mut().filter(|cfd| Cfd::is_cleanup(cfd)) {
        cfd.state = CfdState::setup_failed(format!(
            "Was in state {} which cannot be continued.",
            cfd.state
        ));

        append_cfd_state(cfd, conn).await?;
    }

    Ok(())
}

pub async fn rebroadcast_transactions(
    conn: &mut PoolConnection<Sqlite>,
    wallet: &Wallet,
) -> Result<()> {
    let cfds = load_all_cfds(conn).await?;

    for dlc in cfds.iter().filter_map(|cfd| Cfd::pending_open_dlc(cfd)) {
        rebroadcast(dlc.lock.0.clone(), wallet, "lock").await;
    }

    for cfd in cfds.iter().filter(|cfd| Cfd::is_must_refund(cfd)) {
        let signed_refund_tx = cfd.refund_tx()?;
        rebroadcast(signed_refund_tx, wallet, "refund").await;
    }

    for cfd in cfds.iter().filter(|cfd| Cfd::is_pending_commit(cfd)) {
        let signed_commit_tx = cfd.commit_tx()?;
        rebroadcast(signed_commit_tx, wallet, "commit").await;
    }

    for cfd in cfds.iter().filter(|cfd| Cfd::is_pending_cet(cfd)) {
        // Double question-mark OK because if we are in PendingCet we must have been Ready before
        let signed_cet = cfd.cet()??;
        rebroadcast(signed_cet, wallet, "cet").await;
    }

    Ok(())
}

async fn rebroadcast(tx: Transaction, wallet: &Wallet, label: &str) {
    let txid = tx.txid();
    match wallet.try_broadcast_transaction(tx).await {
        Ok(txid) => tracing::info!(tx=%txid, "{} transaction published", label),
        Err(e) => {
            tracing::error!(tx=%txid, "Failed to re-broadcast {} transaction: {:#}", label, e)
        }
    }
}
