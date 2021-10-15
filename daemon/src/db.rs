use crate::model::cfd::{Cfd, CfdState, Order, OrderId, Origin};
use crate::model::{BitMexPriceEventId, Leverage, Position};
use anyhow::{Context, Result};
use rocket_db_pools::sqlx;
use sqlx::pool::PoolConnection;
use sqlx::{Acquire, Sqlite, SqlitePool};
use std::convert::TryInto;
use std::mem;

pub async fn run_migrations(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

pub async fn insert_order(order: &Order, conn: &mut PoolConnection<Sqlite>) -> anyhow::Result<()> {
    let uuid = serde_json::to_string(&order.id).unwrap();
    let trading_pair = serde_json::to_string(&order.trading_pair).unwrap();
    let position = serde_json::to_string(&order.position).unwrap();
    let initial_price = serde_json::to_string(&order.price).unwrap();
    let min_quantity = serde_json::to_string(&order.min_quantity).unwrap();
    let max_quantity = serde_json::to_string(&order.max_quantity).unwrap();
    let leverage = order.leverage.0;
    let liquidation_price = serde_json::to_string(&order.liquidation_price).unwrap();
    let creation_timestamp = serde_json::to_string(&order.creation_timestamp).unwrap();
    let term = serde_json::to_string(&order.term).unwrap();
    let origin = serde_json::to_string(&order.origin).unwrap();
    let oracle_event_id = order.oracle_event_id.to_string();

    sqlx::query!(
        r#"
            insert into orders (
                uuid,
                trading_pair,
                position,
                initial_price,
                min_quantity,
                max_quantity,
                leverage,
                liquidation_price,
                creation_timestamp,
                term,
                origin,
                oracle_event_id
            ) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?);
            "#,
        uuid,
        trading_pair,
        position,
        initial_price,
        min_quantity,
        max_quantity,
        leverage,
        liquidation_price,
        creation_timestamp,
        term,
        origin,
        oracle_event_id
    )
    .execute(conn)
    .await?;

    Ok(())
}

pub async fn load_order_by_id(
    id: OrderId,
    conn: &mut PoolConnection<Sqlite>,
) -> anyhow::Result<Order> {
    let uuid = serde_json::to_string(&id).unwrap();

    let row = sqlx::query!(
        r#"
        select * from orders where uuid = ?;
        "#,
        uuid
    )
    .fetch_one(conn)
    .await?;

    let uuid = serde_json::from_str(row.uuid.as_str()).unwrap();
    let trading_pair = serde_json::from_str(row.trading_pair.as_str()).unwrap();
    let position = serde_json::from_str(row.position.as_str()).unwrap();
    let initial_price = serde_json::from_str(row.initial_price.as_str()).unwrap();
    let min_quantity = serde_json::from_str(row.min_quantity.as_str()).unwrap();
    let max_quantity = serde_json::from_str(row.max_quantity.as_str()).unwrap();
    let leverage = Leverage(row.leverage.try_into().unwrap());
    let liquidation_price = serde_json::from_str(row.liquidation_price.as_str()).unwrap();
    let creation_timestamp = serde_json::from_str(row.creation_timestamp.as_str()).unwrap();
    let term = serde_json::from_str(row.term.as_str()).unwrap();
    let origin = serde_json::from_str(row.origin.as_str()).unwrap();

    Ok(Order {
        id: uuid,
        trading_pair,
        position,
        price: initial_price,
        min_quantity,
        max_quantity,
        leverage,
        liquidation_price,
        creation_timestamp,
        term,
        origin,
        oracle_event_id: row.oracle_event_id.parse().unwrap(),
    })
}

pub async fn insert_cfd(cfd: &Cfd, conn: &mut PoolConnection<Sqlite>) -> anyhow::Result<()> {
    let mut tx = conn.begin().await?;

    let order_uuid = serde_json::to_string(&cfd.order.id)?;
    let order_row = sqlx::query!(
        r#"
        select * from orders where uuid = ?;
        "#,
        order_uuid
    )
    .fetch_one(&mut tx)
    .await?;

    let order_id = order_row.id;
    let quantity_usd = serde_json::to_string(&cfd.quantity_usd)?;

    let cfd_state = serde_json::to_string(&cfd.state)?;

    // save cfd + state in a transaction to make sure the state is only inserted if the cfd was
    // inserted

    let cfd_id = sqlx::query!(
        r#"
            insert into cfds (
                order_id,
                order_uuid,
                quantity_usd
            ) values (?, ?, ?);
            "#,
        order_id,
        order_uuid,
        quantity_usd,
    )
    .execute(&mut tx)
    .await?
    .last_insert_rowid();

    sqlx::query!(
        r#"
            insert into cfd_states (
                cfd_id,
                state
            ) values (?, ?);
            "#,
        cfd_id,
        cfd_state,
    )
    .execute(&mut tx)
    .await?;

    tx.commit().await?;

    Ok(())
}

pub async fn append_cfd_state(cfd: &Cfd, conn: &mut PoolConnection<Sqlite>) -> anyhow::Result<()> {
    let cfd_id = load_cfd_id_by_order_uuid(cfd.order.id, conn).await?;
    let current_state = load_latest_cfd_state(cfd_id, conn)
        .await
        .context("loading latest state failed")?;
    let new_state = &cfd.state;

    if mem::discriminant(&current_state) == mem::discriminant(new_state) {
        // Since we have states where we add information this happens quite frequently
        tracing::trace!(
            "Same state transition for cfd with order_id {}: {}",
            cfd.order.id,
            current_state
        );
    }

    let cfd_state = serde_json::to_string(new_state)?;

    sqlx::query!(
        r#"
        insert into cfd_states (
            cfd_id,
            state
        ) values (?, ?);
        "#,
        cfd_id,
        cfd_state,
    )
    .execute(conn)
    .await?;

    Ok(())
}

#[allow(dead_code)]
async fn load_cfd_id_by_order_uuid(
    order_uuid: OrderId,
    conn: &mut PoolConnection<Sqlite>,
) -> anyhow::Result<i64> {
    let order_uuid = serde_json::to_string(&order_uuid)?;

    let cfd_id = sqlx::query!(
        r#"
        select
            id
        from cfds
        where order_uuid = ?;
        "#,
        order_uuid
    )
    .fetch_one(conn)
    .await?;

    let cfd_id = cfd_id.id.context("No cfd found")?;

    Ok(cfd_id)
}

#[allow(dead_code)]
async fn load_latest_cfd_state(
    cfd_id: i64,
    conn: &mut PoolConnection<Sqlite>,
) -> anyhow::Result<CfdState> {
    let latest_cfd_state = sqlx::query!(
        r#"
        select
            state
        from cfd_states
        where cfd_id = ?
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

pub async fn load_cfd_by_order_id(
    order_id: OrderId,
    conn: &mut PoolConnection<Sqlite>,
) -> Result<Cfd> {
    let order_uuid = serde_json::to_string(&order_id)?;

    let row = sqlx::query!(
        r#"
        select
            orders.uuid as order_id,
            orders.initial_price as price,
            orders.min_quantity as min_quantity,
            orders.max_quantity as max_quantity,
            orders.leverage as leverage,
            orders.trading_pair as trading_pair,
            orders.position as position,
            orders.origin as origin,
            orders.liquidation_price as liquidation_price,
            orders.creation_timestamp as creation_timestamp,
            orders.term as term,
            orders.oracle_event_id,
            cfds.quantity_usd as quantity_usd,
            cfd_states.state as state
        from cfds as cfds
        inner join orders as orders on cfds.order_id = orders.id
        inner join cfd_states as cfd_states on cfd_states.cfd_id = cfds.id
        where cfd_states.state in (
            select
              state
              from cfd_states
            where cfd_id = cfds.id
            order by id desc
            limit 1
        )
        and orders.uuid = ?
        "#,
        order_uuid
    )
    .fetch_one(conn)
    .await?;

    let order_id = serde_json::from_str(row.order_id.as_str()).unwrap();
    let trading_pair = serde_json::from_str(row.trading_pair.as_str()).unwrap();
    let position: Position = serde_json::from_str(row.position.as_str()).unwrap();
    let price = serde_json::from_str(row.price.as_str()).unwrap();
    let min_quantity = serde_json::from_str(row.min_quantity.as_str()).unwrap();
    let max_quantity = serde_json::from_str(row.max_quantity.as_str()).unwrap();
    let leverage = Leverage(row.leverage.try_into().unwrap());
    let liquidation_price = serde_json::from_str(row.liquidation_price.as_str()).unwrap();
    let creation_timestamp = serde_json::from_str(row.creation_timestamp.as_str()).unwrap();
    let term = serde_json::from_str(row.term.as_str()).unwrap();
    let origin: Origin = serde_json::from_str(row.origin.as_str()).unwrap();
    let oracle_event_id = row.oracle_event_id.parse().unwrap();

    let quantity = serde_json::from_str(row.quantity_usd.as_str()).unwrap();
    let latest_state = serde_json::from_str(row.state.as_str()).unwrap();

    let order = Order {
        id: order_id,
        trading_pair,
        position,
        price,
        min_quantity,
        max_quantity,
        leverage,
        liquidation_price,
        creation_timestamp,
        term,
        origin,
        oracle_event_id,
    };

    Ok(Cfd {
        order,
        quantity_usd: quantity,
        state: latest_state,
    })
}

/// Loads all CFDs with the latest state as the CFD state
pub async fn load_all_cfds(conn: &mut PoolConnection<Sqlite>) -> anyhow::Result<Vec<Cfd>> {
    // TODO: Could be optimized with something like but not sure it's worth the complexity:

    let rows = sqlx::query!(
        r#"
        select
            orders.uuid as order_id,
            orders.initial_price as price,
            orders.min_quantity as min_quantity,
            orders.max_quantity as max_quantity,
            orders.leverage as leverage,
            orders.trading_pair as trading_pair,
            orders.position as position,
            orders.origin as origin,
            orders.liquidation_price as liquidation_price,
            orders.creation_timestamp as creation_timestamp,
            orders.term as term,
            orders.oracle_event_id,
            cfds.quantity_usd as quantity_usd,
            cfd_states.state as state
        from cfds as cfds
        inner join orders as orders on cfds.order_id = orders.id
        inner join cfd_states as cfd_states on cfd_states.cfd_id = cfds.id
        where cfd_states.state in (
            select
              state
              from cfd_states
            where cfd_id = cfds.id
            order by id desc
            limit 1
        )
        "#
    )
    .fetch_all(conn)
    .await?;

    let cfds = rows
        .iter()
        .map(|row| {
            let order_id = serde_json::from_str(row.order_id.as_str()).unwrap();
            let trading_pair = serde_json::from_str(row.trading_pair.as_str()).unwrap();
            let position: Position = serde_json::from_str(row.position.as_str()).unwrap();
            let price = serde_json::from_str(row.price.as_str()).unwrap();
            let min_quantity = serde_json::from_str(row.min_quantity.as_str()).unwrap();
            let max_quantity = serde_json::from_str(row.max_quantity.as_str()).unwrap();
            let leverage = Leverage(row.leverage.try_into().unwrap());
            let liquidation_price = serde_json::from_str(row.liquidation_price.as_str()).unwrap();
            let creation_timestamp = serde_json::from_str(row.creation_timestamp.as_str()).unwrap();
            let term = serde_json::from_str(row.term.as_str()).unwrap();
            let origin: Origin = serde_json::from_str(row.origin.as_str()).unwrap();
            let oracle_event_id = row.oracle_event_id.clone().parse().unwrap();

            let quantity = serde_json::from_str(row.quantity_usd.as_str()).unwrap();
            let latest_state = serde_json::from_str(row.state.as_str()).unwrap();

            let order = Order {
                id: order_id,
                trading_pair,
                position,
                price,
                min_quantity,
                max_quantity,
                leverage,
                liquidation_price,
                creation_timestamp,
                term,
                origin,
                oracle_event_id,
            };

            Cfd {
                order,
                quantity_usd: quantity,
                state: latest_state,
            }
        })
        .collect();

    Ok(cfds)
}

/// Loads all CFDs with the latest state as the CFD state
pub async fn load_cfds_by_oracle_event_id(
    oracle_event_id: BitMexPriceEventId,
    conn: &mut PoolConnection<Sqlite>,
) -> anyhow::Result<Vec<Cfd>> {
    let oracle_event_id = oracle_event_id.to_string();

    let rows = sqlx::query!(
        r#"
        select
            orders.uuid as order_id,
            orders.initial_price as price,
            orders.min_quantity as min_quantity,
            orders.max_quantity as max_quantity,
            orders.leverage as leverage,
            orders.trading_pair as trading_pair,
            orders.position as position,
            orders.origin as origin,
            orders.liquidation_price as liquidation_price,
            orders.creation_timestamp as creation_timestamp,
            orders.term as term,
            orders.oracle_event_id,
            cfds.quantity_usd as quantity_usd,
            cfd_states.state as state
        from cfds as cfds
        inner join orders as orders on cfds.order_id = orders.id
        inner join cfd_states as cfd_states on cfd_states.cfd_id = cfds.id
        where cfd_states.state in (
            select
              state
              from cfd_states
            where cfd_id = cfds.id
            order by id desc
            limit 1
        )
        and orders.oracle_event_id = ?
        "#,
        oracle_event_id
    )
    .fetch_all(conn)
    .await?;

    let cfds = rows
        .iter()
        .map(|row| {
            let order_id = serde_json::from_str(row.order_id.as_str()).unwrap();
            let trading_pair = serde_json::from_str(row.trading_pair.as_str()).unwrap();
            let position: Position = serde_json::from_str(row.position.as_str()).unwrap();
            let price = serde_json::from_str(row.price.as_str()).unwrap();
            let min_quantity = serde_json::from_str(row.min_quantity.as_str()).unwrap();
            let max_quantity = serde_json::from_str(row.max_quantity.as_str()).unwrap();
            let leverage = Leverage(row.leverage.try_into().unwrap());
            let liquidation_price = serde_json::from_str(row.liquidation_price.as_str()).unwrap();
            let creation_timestamp = serde_json::from_str(row.creation_timestamp.as_str()).unwrap();
            let term = serde_json::from_str(row.term.as_str()).unwrap();
            let origin: Origin = serde_json::from_str(row.origin.as_str()).unwrap();
            let oracle_event_id = row.oracle_event_id.parse().unwrap();

            let quantity = serde_json::from_str(row.quantity_usd.as_str()).unwrap();
            let latest_state = serde_json::from_str(row.state.as_str()).unwrap();

            let order = Order {
                id: order_id,
                trading_pair,
                position,
                price,
                min_quantity,
                max_quantity,
                leverage,
                liquidation_price,
                creation_timestamp,
                term,
                origin,
                oracle_event_id,
            };

            Cfd {
                order,
                quantity_usd: quantity,
                state: latest_state,
            }
        })
        .collect();

    Ok(cfds)
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use pretty_assertions::assert_eq;
    use rand::Rng;
    use rust_decimal_macros::dec;
    use sqlx::SqlitePool;
    use tempfile::tempdir;
    use time::macros::datetime;
    use time::OffsetDateTime;

    use crate::db::insert_order;
    use crate::model::cfd::{Cfd, CfdState, Order};
    use crate::model::Usd;

    use super::*;

    #[tokio::test]
    async fn test_insert_and_load_order() {
        let mut conn = setup_test_db().await;

        let order = Order::dummy().insert(&mut conn).await;
        let loaded = load_order_by_id(order.id, &mut conn).await.unwrap();

        assert_eq!(order, loaded);
    }

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
        let loaded = load_cfd_by_order_id(cfd.order.id, &mut conn).await.unwrap();

        assert_eq!(cfd, loaded)
    }

    #[tokio::test]
    async fn test_insert_and_load_cfd_by_order_id_multiple() {
        let mut conn = setup_test_db().await;

        let cfd1 = Cfd::dummy().insert(&mut conn).await;
        let cfd2 = Cfd::dummy().insert(&mut conn).await;

        let loaded_1 = load_cfd_by_order_id(cfd1.order.id, &mut conn)
            .await
            .unwrap();
        let loaded_2 = load_cfd_by_order_id(cfd2.order.id, &mut conn)
            .await
            .unwrap();

        assert_eq!(cfd1, loaded_1);
        assert_eq!(cfd2, loaded_2);
    }

    #[tokio::test]
    async fn test_insert_and_load_cfd_by_oracle_event_id() {
        let mut conn = setup_test_db().await;

        let cfd_1 = Cfd::dummy()
            .with_event_id(BitMexPriceEventId::event1())
            .insert(&mut conn)
            .await;
        let cfd_2 = Cfd::dummy()
            .with_event_id(BitMexPriceEventId::event1())
            .insert(&mut conn)
            .await;
        let cfd_3 = Cfd::dummy()
            .with_event_id(BitMexPriceEventId::event2())
            .insert(&mut conn)
            .await;

        let cfds_event_1 = load_cfds_by_oracle_event_id(BitMexPriceEventId::event1(), &mut conn)
            .await
            .unwrap();

        let cfds_event_2 = load_cfds_by_oracle_event_id(BitMexPriceEventId::event2(), &mut conn)
            .await
            .unwrap();

        assert_eq!(vec![cfd_1, cfd_2], cfds_event_1);
        assert_eq!(vec![cfd_3], cfds_event_2);
    }

    #[tokio::test]
    async fn test_insert_new_cfd_state_and_load_with_multiple_cfd() {
        let mut conn = setup_test_db().await;

        let mut cfd_1 = Cfd::dummy().insert(&mut conn).await;

        cfd_1.state = CfdState::accepted();
        append_cfd_state(&cfd_1, &mut conn).await.unwrap();

        let cfds_from_db = load_all_cfds(&mut conn).await.unwrap();
        assert_eq!(vec![cfd_1.clone()], cfds_from_db);

        let mut cfd_2 = Cfd::dummy().insert(&mut conn).await;

        let cfds_from_db = load_all_cfds(&mut conn).await.unwrap();
        assert_eq!(vec![cfd_1.clone(), cfd_2.clone()], cfds_from_db);

        cfd_2.state = CfdState::rejected();
        append_cfd_state(&cfd_2, &mut conn).await.unwrap();

        let cfds_from_db = load_all_cfds(&mut conn).await.unwrap();
        assert_eq!(vec![cfd_1, cfd_2], cfds_from_db);
    }

    // test more data; test will add 100 cfds to the database, with each
    // having a random number of random updates. Final results are deterministic.
    #[tokio::test]
    async fn test_multiple_cfd_updates_per_cfd() {
        let mut conn = setup_test_db().await;

        for _ in 0..100 {
            let mut cfd = Cfd::dummy().insert(&mut conn).await;

            let n_updates = rand::thread_rng().gen_range(1, 30);

            for _ in 0..n_updates {
                cfd.state = random_simple_state();
                append_cfd_state(&cfd, &mut conn).await.unwrap();
            }

            // verify current state is correct
            let loaded = load_cfd_by_order_id(cfd.order.id, &mut conn).await.unwrap();
            assert_eq!(loaded, cfd);
        }

        // verify query returns only one state per CFD
        let data = load_all_cfds(&mut conn).await.unwrap();

        assert_eq!(data.len(), 100);
    }

    fn random_simple_state() -> CfdState {
        match rand::thread_rng().gen_range(0, 5) {
            0 => CfdState::outgoing_order_request(),
            1 => CfdState::accepted(),
            2 => CfdState::rejected(),
            3 => CfdState::contract_setup(),
            _ => CfdState::setup_failed(String::from("dummy failure")),
        }
    }

    async fn setup_test_db() -> PoolConnection<Sqlite> {
        let temp_db = tempdir().unwrap().into_path().join("tempdb");

        // file has to exist in order to connect with sqlite
        File::create(temp_db.clone()).unwrap();

        let pool = SqlitePool::connect(format!("sqlite:{}", temp_db.display()).as_str())
            .await
            .unwrap();

        run_migrations(&pool).await.unwrap();

        pool.acquire().await.unwrap()
    }

    impl BitMexPriceEventId {
        fn event1() -> Self {
            BitMexPriceEventId::with_20_digits(datetime!(2021-10-13 10:00:00).assume_utc())
        }

        fn event2() -> Self {
            BitMexPriceEventId::with_20_digits(datetime!(2021-10-25 18:00:00).assume_utc())
        }
    }

    impl Cfd {
        fn dummy() -> Self {
            Cfd::new(
                Order::dummy(),
                Usd(dec!(1000)),
                CfdState::outgoing_order_request(),
            )
        }

        /// Insert this [`Cfd`] into the database, returning the instance for further chaining.
        async fn insert(self, conn: &mut PoolConnection<Sqlite>) -> Self {
            insert_order(&self.order, conn).await.unwrap();
            insert_cfd(&self, conn).await.unwrap();

            self
        }

        fn with_event_id(mut self, id: BitMexPriceEventId) -> Self {
            self.order.oracle_event_id = id;
            self
        }
    }

    impl Order {
        fn dummy() -> Self {
            Order::new(
                Usd(dec!(1000)),
                Usd(dec!(100)),
                Usd(dec!(1000)),
                Origin::Theirs,
                BitMexPriceEventId::with_20_digits(OffsetDateTime::now_utc()),
            )
            .unwrap()
        }

        /// Insert this [`Order`] into the database, returning the instance for further chaining.
        async fn insert(self, conn: &mut PoolConnection<Sqlite>) -> Self {
            insert_order(&self, conn).await.unwrap();

            self
        }
    }
}
