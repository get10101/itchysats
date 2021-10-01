use crate::db::{insert_new_cfd_state_by_order_id, load_all_cfds};
use crate::model::cfd::{Cfd, CfdState, CfdStateCommon};
use anyhow::Result;
use sqlx::pool::PoolConnection;
use sqlx::Sqlite;
use std::time::SystemTime;

pub async fn transition_non_continue_cfds_to_setup_failed(
    conn: &mut PoolConnection<Sqlite>,
) -> Result<()> {
    let cfds = load_all_cfds(conn).await?;

    for cfd in cfds.iter().filter(|cfd| Cfd::is_cleanup(cfd)) {
        insert_new_cfd_state_by_order_id(
            cfd.order.id,
            CfdState::SetupFailed {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
                info: format!("Was in state {} which cannot be continued.", cfd.state),
            },
            conn,
        )
        .await?;
    }

    Ok(())
}
