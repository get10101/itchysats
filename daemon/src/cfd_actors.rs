use crate::db;
use crate::projection;
use anyhow::Result;
use model::Cfd;
use model::OrderId;
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

/// Load a CFD from the database and rehydrate as the [`model::cfd::Cfd`] aggregate.
pub async fn load_cfd(order_id: OrderId, conn: &mut PoolConnection<Sqlite>) -> Result<Cfd> {
    let (
        db::Cfd {
            id,
            position,
            initial_price,
            taker_leverage: leverage,
            settlement_interval,
            counterparty_network_identity,
            role,
            quantity_usd,
            opening_fee,
            initial_funding_rate,
            initial_tx_fee_rate,
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
        opening_fee,
        initial_funding_rate,
        initial_tx_fee_rate,
        events,
    );
    Ok(cfd)
}
