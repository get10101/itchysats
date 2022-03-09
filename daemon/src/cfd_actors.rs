use crate::db;
use crate::projection;
use anyhow::Result;
use model::Cfd;
use sqlx::pool::PoolConnection;
use sqlx::Sqlite;

pub async fn insert_cfd_and_update_feed(
    cfd: &Cfd,
    conn: &mut PoolConnection<Sqlite>,
    projection_address: &xtra::Address<projection::Actor>,
) -> Result<()> {
    db::insert_cfd(cfd, conn).await?;
    projection_address
        .send(projection::CfdChanged(cfd.id()))
        .await?;
    Ok(())
}
