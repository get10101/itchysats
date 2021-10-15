use crate::db::{append_cfd_state, load_all_cfds};
use crate::model::cfd::{Cfd, CfdState, CfdStateCommon};
use crate::wallet::Wallet;
use anyhow::Result;
use sqlx::pool::PoolConnection;
use sqlx::Sqlite;

pub async fn transition_non_continue_cfds_to_setup_failed(
    conn: &mut PoolConnection<Sqlite>,
) -> Result<()> {
    let mut cfds = load_all_cfds(conn).await?;

    for cfd in cfds.iter_mut().filter(|cfd| Cfd::is_cleanup(cfd)) {
        cfd.state = CfdState::SetupFailed {
            common: CfdStateCommon::default(),
            info: format!("Was in state {} which cannot be continued.", cfd.state),
        };

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
        let txid = wallet.try_broadcast_transaction(dlc.lock.0.clone()).await?;

        tracing::info!("Lock transaction published with txid {}", txid);
    }

    for cfd in cfds.iter().filter(|cfd| Cfd::is_must_refund(cfd)) {
        let signed_refund_tx = cfd.refund_tx()?;
        let txid = wallet.try_broadcast_transaction(signed_refund_tx).await?;

        tracing::info!("Refund transaction published on chain: {}", txid);
    }

    for cfd in cfds.iter().filter(|cfd| Cfd::is_pending_commit(cfd)) {
        let signed_commit_tx = cfd.commit_tx()?;
        let txid = wallet.try_broadcast_transaction(signed_commit_tx).await?;

        tracing::info!("Commit transaction published on chain: {}", txid);
    }

    for cfd in cfds.iter().filter(|cfd| Cfd::is_pending_cet(cfd)) {
        // Double question-mark OK because if we are in PendingCet we must have been Ready before
        let signed_cet = cfd.cet()??;
        let txid = wallet.try_broadcast_transaction(signed_cet).await?;

        tracing::info!("CET published on chain: {}", txid);
    }

    Ok(())
}
