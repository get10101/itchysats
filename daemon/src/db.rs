use crate::model::cfd::Cfd;
use crate::model::cfd::CfdState;
use crate::model::cfd::OrderId;
use anyhow::Context;
use anyhow::Result;
use sqlx::pool::PoolConnection;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use std::mem;
use time::Duration;

pub async fn run_migrations(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

pub async fn insert_cfd(cfd: &Cfd, conn: &mut PoolConnection<Sqlite>) -> anyhow::Result<()> {
    let state = serde_json::to_string(&cfd.state())?;
    let query_result = sqlx::query(
        r#"
        insert into cfds (
            uuid,
            trading_pair,
            position,
            initial_price,
            leverage,
            liquidation_price,
            creation_timestamp_seconds,
            settlement_time_interval_seconds,
            role,
            fee_rate,
            quantity_usd,
            counterparty
        ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12);

        insert into cfd_states (
            cfd_id,
            state
        )
        select
            id as cfd_id,
            $13 as state
        from cfds
        order by id desc limit 1;
        "#,
    )
    .bind(&cfd.id())
    .bind(&cfd.trading_pair())
    .bind(&cfd.position())
    .bind(&cfd.price())
    .bind(&cfd.leverage())
    .bind(&cfd.liquidation_price())
    .bind(&cfd.creation_timestamp())
    .bind(&cfd.settlement_interval().whole_seconds())
    .bind(&cfd.role())
    .bind(&cfd.fee_rate())
    .bind(&cfd.quantity_usd())
    .bind(&cfd.counterparty())
    .bind(state)
    .execute(conn)
    .await
    .with_context(|| format!("Failed to insert CFD with id {}", cfd.id()))?;

    // Should be 2 because we insert into cfds and cfd_states
    if query_result.rows_affected() != 2 {
        anyhow::bail!("failed to insert cfd");
    }

    Ok(())
}

pub async fn append_cfd_state(cfd: &Cfd, conn: &mut PoolConnection<Sqlite>) -> anyhow::Result<()> {
    let cfd_id = load_cfd_id_by_order_uuid(cfd.id(), conn).await?;
    let current_state = load_latest_cfd_state(cfd_id, conn)
        .await
        .context("loading latest state failed")?;
    let new_state = &cfd.state();

    if mem::discriminant(&current_state) == mem::discriminant(new_state) {
        // Since we have states where we add information this happens quite frequently
        tracing::trace!(
            "Same state transition for cfd with order_id {}: {}",
            cfd.id(),
            current_state
        );
    }

    let cfd_state = serde_json::to_string(new_state)?;

    sqlx::query(
        r#"
        insert into cfd_states (
            cfd_id,
            state
        ) values ($1, $2);
        "#,
    )
    .bind(cfd_id)
    .bind(cfd_state)
    .execute(conn)
    .await?;

    Ok(())
}

async fn load_cfd_id_by_order_uuid(
    order_uuid: OrderId,
    conn: &mut PoolConnection<Sqlite>,
) -> anyhow::Result<i64> {
    let cfd_id = sqlx::query!(
        r#"
        select
            id
        from cfds
        where cfds.uuid = $1;
        "#,
        order_uuid
    )
    .fetch_one(conn)
    .await?;

    let cfd_id = cfd_id.id.context("No cfd found")?;

    Ok(cfd_id)
}

async fn load_latest_cfd_state(
    cfd_id: i64,
    conn: &mut PoolConnection<Sqlite>,
) -> anyhow::Result<CfdState> {
    let latest_cfd_state = sqlx::query!(
        r#"
        select
            state
        from cfd_states
        where cfd_id = $1
        order by id desc
        limit 1;
        "#,
        cfd_id
    )
    .fetch_one(conn)
    .await?;

    let latest_cfd_state_in_db: CfdState = serde_json::from_str(latest_cfd_state.state.as_str())?;

    Ok(latest_cfd_state_in_db)
}

pub async fn load_cfd(order_id: OrderId, conn: &mut PoolConnection<Sqlite>) -> Result<Cfd> {
    let row = sqlx::query!(
        r#"
        with state as (
            select
                cfd_id,
                state
            from cfd_states
                inner join cfds on cfds.id = cfd_states.cfd_id
            where cfd_states.id in (
                select
                    max(id) as id
                from cfd_states
                group by (cfd_id)
            )
        )

        select
            cfds.uuid as "uuid: crate::model::cfd::OrderId",
            cfds.trading_pair as "trading_pair: crate::model::TradingPair",
            cfds.position as "position: crate::model::Position",
            cfds.initial_price as "initial_price: crate::model::Price",
            cfds.leverage as "leverage: crate::model::Leverage",
            cfds.liquidation_price as "liquidation_price: crate::model::Price",
            cfds.creation_timestamp_seconds as "creation_timestamp_seconds: crate::model::Timestamp",
            cfds.settlement_time_interval_seconds as "settlement_time_interval_secs: i64",
            cfds.role as "role: crate::model::cfd::Role",
            cfds.fee_rate as "fee_rate: u32",
            cfds.quantity_usd as "quantity_usd: crate::model::Usd",
            cfds.counterparty as "counterparty: crate::model::Identity",
            state.state

        from cfds
            inner join state on state.cfd_id = cfds.id

        where cfds.uuid = $1
        "#,
        order_id
    )
    .fetch_one(conn)
    .await?;

    // TODO:
    // still have the use of serde_json::from_str() here, which will be dealt with
    // via https://github.com/comit-network/hermes/issues/290
    Ok(Cfd::new(
        row.uuid,
        row.trading_pair,
        row.position,
        row.initial_price,
        row.leverage,
        row.liquidation_price,
        row.creation_timestamp_seconds,
        Duration::new(row.settlement_time_interval_secs, 0),
        row.role,
        row.fee_rate,
        row.quantity_usd,
        serde_json::from_str(row.state.as_str())?,
        row.counterparty,
    ))
}

/// Loads all CFDs with the latest state as the CFD state
pub async fn load_all_cfds(conn: &mut PoolConnection<Sqlite>) -> anyhow::Result<Vec<Cfd>> {
    let rows = sqlx::query!(
        r#"
        with state as (
            select
                cfd_id,
                state
            from cfd_states
                inner join cfds on cfds.id = cfd_states.cfd_id
            where cfd_states.id in (
                select
                    max(id) as id
                from cfd_states
                group by (cfd_id)
            )
        )

        select
            cfds.uuid as "uuid: crate::model::cfd::OrderId",
            cfds.trading_pair as "trading_pair: crate::model::TradingPair",
            cfds.position as "position: crate::model::Position",
            cfds.initial_price as "initial_price: crate::model::Price",
            cfds.leverage as "leverage: crate::model::Leverage",
            cfds.liquidation_price as "liquidation_price: crate::model::Price",
            cfds.creation_timestamp_seconds as "creation_timestamp_seconds: crate::model::Timestamp",
            cfds.settlement_time_interval_seconds as "settlement_time_interval_secs: i64",
            cfds.role as "role: crate::model::cfd::Role",
            cfds.fee_rate as "fee_rate: u32",
            cfds.quantity_usd as "quantity_usd: crate::model::Usd",
            cfds.counterparty as "counterparty: crate::model::Identity",
            state.state

        from cfds
            inner join state on state.cfd_id = cfds.id
        "#
    )
    .fetch_all(conn)
    .await?;

    let cfds = rows
        .into_iter()
        .map(|row| {
            Ok(Cfd::new(
                row.uuid,
                row.trading_pair,
                row.position,
                row.initial_price,
                row.leverage,
                row.liquidation_price,
                row.creation_timestamp_seconds,
                Duration::new(row.settlement_time_interval_secs, 0),
                row.role,
                row.fee_rate,
                row.quantity_usd,
                serde_json::from_str(row.state.as_str())?,
                row.counterparty,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(cfds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::cfd::Cfd;
    use crate::model::cfd::CfdState;
    use crate::model::cfd::Role;
    use crate::model::Identity;
    use crate::model::Leverage;
    use crate::model::Position;
    use crate::model::Price;
    use crate::model::Timestamp;
    use crate::model::TradingPair;
    use crate::model::Usd;
    use crate::seed::Seed;
    use crate::SETTLEMENT_INTERVAL;
    use pretty_assertions::assert_eq;
    use rust_decimal_macros::dec;
    use sqlx::SqlitePool;

    #[tokio::test]
    async fn test_insert_and_load_cfd() {
        let mut conn = setup_test_db().await;

        let cfd = Cfd::dummy().insert(&mut conn).await;
        let loaded = load_all_cfds(&mut conn).await.unwrap();

        assert_eq!(vec![cfd], loaded);
    }

    #[tokio::test]
    async fn test_insert_and_load_cfd_by_order_id() {
        let mut conn = setup_test_db().await;

        let cfd = Cfd::dummy().insert(&mut conn).await;
        let loaded = load_cfd(cfd.id(), &mut conn).await.unwrap();

        assert_eq!(cfd, loaded)
    }

    #[tokio::test]
    async fn test_insert_and_load_cfd_by_order_id_multiple() {
        let mut conn = setup_test_db().await;

        let cfd1 = Cfd::dummy().insert(&mut conn).await;
        let cfd2 = Cfd::dummy().insert(&mut conn).await;

        let loaded_1 = load_cfd(cfd1.id(), &mut conn).await.unwrap();
        let loaded_2 = load_cfd(cfd2.id(), &mut conn).await.unwrap();

        assert_eq!(cfd1, loaded_1);
        assert_eq!(cfd2, loaded_2);
    }

    #[tokio::test]
    async fn test_insert_new_cfd_state_and_load_with_multiple_cfd() {
        let mut conn = setup_test_db().await;

        let mut cfd_1 = Cfd::dummy().insert(&mut conn).await;

        *cfd_1.state_mut() = CfdState::accepted();
        append_cfd_state(&cfd_1, &mut conn).await.unwrap();

        let cfds_from_db = load_all_cfds(&mut conn).await.unwrap();
        assert_eq!(vec![cfd_1.clone()], cfds_from_db);

        let mut cfd_2 = Cfd::dummy().insert(&mut conn).await;

        let cfds_from_db = load_all_cfds(&mut conn).await.unwrap();
        assert_eq!(vec![cfd_1.clone(), cfd_2.clone()], cfds_from_db);

        *cfd_2.state_mut() = CfdState::rejected();
        append_cfd_state(&cfd_2, &mut conn).await.unwrap();

        let cfds_from_db = load_all_cfds(&mut conn).await.unwrap();
        assert_eq!(vec![cfd_1, cfd_2], cfds_from_db);
    }

    #[tokio::test]
    async fn inserting_two_cfds_with_same_order_id_should_fail() {
        let mut conn = setup_test_db().await;

        let cfd = Cfd::dummy().insert(&mut conn).await;

        let error = insert_cfd(&cfd, &mut conn).await.err().unwrap();
        assert_eq!(
            format!("{:#}", error),
            format!(
                "Failed to insert CFD with id {}: error returned from database: UNIQUE constraint failed: cfds.uuid: UNIQUE constraint failed: cfds.uuid",
                cfd.id(),
            )
        );
    }

    async fn setup_test_db() -> PoolConnection<Sqlite> {
        let pool = SqlitePool::connect(":memory:").await.unwrap();

        run_migrations(&pool).await.unwrap();

        pool.acquire().await.unwrap()
    }

    impl Cfd {
        fn dummy() -> Self {
            let (pub_key, _) = Seed::default().derive_identity();
            let dummy_identity = Identity::new(pub_key);

            Cfd::new(
                OrderId::default(),
                TradingPair::BtcUsd,
                Position::Long,
                Price::new(dec!(1000)).unwrap(),
                Leverage::new(2).unwrap(),
                Price::new(dec!(400)).unwrap(),
                Timestamp::now(),
                SETTLEMENT_INTERVAL,
                Role::Taker,
                1,
                Usd::new(dec!(1000)),
                CfdState::outgoing_order_request(),
                dummy_identity,
            )
        }

        /// Insert this [`Cfd`] into the database, returning the instance for further chaining.
        async fn insert(self, conn: &mut PoolConnection<Sqlite>) -> Self {
            insert_cfd(&self, conn).await.unwrap();

            self
        }
    }
}
