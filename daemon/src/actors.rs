use crate::{
    db,
    model::cfd::{Cfd, CfdState, OrderId},
};
use sqlx::{pool::PoolConnection, Sqlite};
use tokio::sync::watch;

/// Wrapper for handlers to log errors
#[macro_export]
macro_rules! log_error {
    ($future:expr) => {
        if let Err(e) = $future.await {
            tracing::error!("Message handler failed: {:#}", e);
        }
    };
}

pub async fn insert_cfd(
    cfd: Cfd,
    conn: &mut PoolConnection<Sqlite>,
    update_sender: &watch::Sender<Vec<Cfd>>,
) -> anyhow::Result<()> {
    db::insert_cfd(cfd, conn).await?;
    update_sender.send(db::load_all_cfds(conn).await?)?;
    Ok(())
}

pub async fn insert_new_cfd_state_by_order_id(
    order_id: OrderId,
    new_state: &CfdState,
    conn: &mut PoolConnection<Sqlite>,
    update_sender: &watch::Sender<Vec<Cfd>>,
) -> anyhow::Result<()> {
    db::insert_new_cfd_state_by_order_id(order_id, new_state, conn).await?;
    update_sender.send(db::load_all_cfds(conn).await?)?;
    Ok(())
}
