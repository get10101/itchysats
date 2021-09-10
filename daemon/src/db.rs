use std::convert::TryInto;
use std::mem;

use anyhow::Context;
use bdk::bitcoin::Amount;
use rocket_db_pools::sqlx;
use sqlx::pool::PoolConnection;
use sqlx::{Sqlite, SqlitePool};

use crate::model::cfd::{Cfd, CfdOffer, CfdOfferId, CfdState};
use crate::model::{Leverage, Usd};

pub mod maker;
pub mod taker;

pub async fn do_run_migrations(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

pub async fn insert_cfd_offer(cfd_offer: CfdOffer, pool: &SqlitePool) -> anyhow::Result<()> {
    let uuid = serde_json::to_string(&cfd_offer.id).unwrap();
    let trading_pair = serde_json::to_string(&cfd_offer.trading_pair).unwrap();
    let position = serde_json::to_string(&cfd_offer.position).unwrap();
    let initial_price = serde_json::to_string(&cfd_offer.price).unwrap();
    let min_quantity = serde_json::to_string(&cfd_offer.min_quantity).unwrap();
    let max_quantity = serde_json::to_string(&cfd_offer.max_quantity).unwrap();
    let leverage = cfd_offer.leverage.0;
    let liquidation_price = serde_json::to_string(&cfd_offer.liquidation_price).unwrap();
    let creation_timestamp = serde_json::to_string(&cfd_offer.creation_timestamp).unwrap();
    let term = serde_json::to_string(&cfd_offer.term).unwrap();

    sqlx::query!(
        r#"
            insert into offers (
                uuid,
                trading_pair,
                position,
                initial_price,
                min_quantity,
                max_quantity,
                leverage,
                liquidation_price,
                creation_timestamp,
                term
            ) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?);
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
        term
    )
    .execute(pool)
    .await?;

    Ok(())
}

// TODO: Consider refactor the API to consistently present PoolConnections

pub async fn load_offer_by_id_from_conn(
    id: CfdOfferId,
    conn: &mut PoolConnection<Sqlite>,
) -> anyhow::Result<CfdOffer> {
    let uuid = serde_json::to_string(&id).unwrap();

    let row = sqlx::query!(
        r#"
        select * from offers where uuid = ?;
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

    Ok(CfdOffer {
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
    })
}

pub async fn load_offer_by_id(id: CfdOfferId, pool: &SqlitePool) -> anyhow::Result<CfdOffer> {
    let mut connection = pool.acquire().await?;
    load_offer_by_id_from_conn(id, &mut connection).await
}

pub async fn insert_cfd(cfd: Cfd, pool: &SqlitePool) -> anyhow::Result<()> {
    let offer_uuid = serde_json::to_string(&cfd.offer_id)?;
    let offer_row = sqlx::query!(
        r#"
        select * from offers where uuid = ?;
        "#,
        offer_uuid
    )
    .fetch_one(pool)
    .await?;

    let offer_id = offer_row.id;
    let quantity_usd = serde_json::to_string(&cfd.quantity_usd)?;

    let cfd_state = serde_json::to_string(&cfd.state)?;

    // save cfd + state in a transaction to make sure the state is only inserted if the cfd was inserted
    let mut tx = pool.begin().await?;

    let cfd_id = sqlx::query!(
        r#"
            insert into cfds (
                offer_id,
                offer_uuid,
                quantity_usd
            ) values (?, ?, ?);
            "#,
        offer_id,
        offer_uuid,
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

pub async fn insert_new_cfd_state_by_offer_id(
    offer_id: CfdOfferId,
    new_state: CfdState,
    pool: &SqlitePool,
) -> anyhow::Result<()> {
    let cfd_id = load_cfd_id_by_offer_uuid(offer_id, pool).await?;
    let latest_cfd_state_in_db = load_latest_cfd_state(cfd_id, pool)
        .await
        .context("loading latest state failed")?;

    // make sure that the new state is different than the current one to avoid that we save the same state twice
    if mem::discriminant(&latest_cfd_state_in_db) == mem::discriminant(&new_state) {
        anyhow::bail!("Cannot insert new state {} for cfd with order_id {} because it currently already is in state {}", new_state, offer_id, latest_cfd_state_in_db);
    }

    let cfd_state = serde_json::to_string(&new_state)?;

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
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn insert_new_cfd_state(cfd: Cfd, pool: &SqlitePool) -> anyhow::Result<()> {
    insert_new_cfd_state_by_offer_id(cfd.offer_id, cfd.state, pool).await?;

    Ok(())
}

async fn load_cfd_id_by_offer_uuid(
    offer_uuid: CfdOfferId,
    pool: &SqlitePool,
) -> anyhow::Result<i64> {
    let offer_uuid = serde_json::to_string(&offer_uuid)?;

    let cfd_id = sqlx::query!(
        r#"
        select
            id
        from cfds
        where offer_uuid = ?;
        "#,
        offer_uuid
    )
    .fetch_one(pool)
    .await?;

    let cfd_id = cfd_id.id.context("No cfd found")?;

    Ok(cfd_id)
}

async fn load_latest_cfd_state(cfd_id: i64, pool: &SqlitePool) -> anyhow::Result<CfdState> {
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
    .fetch_one(pool)
    .await?;

    let latest_cfd_state_in_db: CfdState =
        serde_json::from_str(dbg!(latest_cfd_state).state.as_str())?;

    Ok(latest_cfd_state_in_db)
}

/// Loads all CFDs with the latest state as the CFD state
pub async fn load_all_cfds(pool: &SqlitePool) -> anyhow::Result<Vec<Cfd>> {
    // TODO: Could be optimized with something like but not sure it's worth the complexity:

    let rows = sqlx::query!(
        r#"
        select
            cfds.id as cfd_id,
            offers.uuid as offer_id,
            offers.initial_price as initial_price,
            offers.leverage as leverage,
            offers.trading_pair as trading_pair,
            offers.position as position,
            offers.liquidation_price as liquidation_price,
            cfds.quantity_usd as quantity_usd,
            cfd_states.state as state
        from cfds as cfds
        inner join offers as offers on cfds.offer_id = offers.id
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
    .fetch_all(pool)
    .await?;

    // TODO: We might want to separate the database model from the http model and properly map between them

    let cfds = rows
        .iter()
        .map(|row| {
            let offer_id = serde_json::from_str(row.offer_id.as_str()).unwrap();
            let initial_price = serde_json::from_str(row.initial_price.as_str()).unwrap();
            let leverage = Leverage(row.leverage.try_into().unwrap());
            let trading_pair = serde_json::from_str(row.trading_pair.as_str()).unwrap();
            let position = serde_json::from_str(row.position.as_str()).unwrap();
            let liquidation_price = serde_json::from_str(row.liquidation_price.as_str()).unwrap();
            let quantity = serde_json::from_str(row.quantity_usd.as_str()).unwrap();
            let latest_state = serde_json::from_str(row.state.as_str()).unwrap();

            Cfd {
                offer_id,
                initial_price,
                leverage,
                trading_pair,
                position,
                liquidation_price,
                quantity_usd: quantity,
                profit_btc: Amount::ZERO,
                profit_usd: Usd::ZERO,
                state: latest_state,
            }
        })
        .collect();

    Ok(cfds)
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::time::SystemTime;

    use rust_decimal_macros::dec;
    use sqlx::SqlitePool;
    use tempfile::tempdir;

    use crate::db::insert_cfd_offer;
    use crate::model::cfd::{Cfd, CfdOffer, CfdState, CfdStateCommon};
    use crate::model::Usd;

    use super::*;

    #[tokio::test]
    async fn test_insert_and_load_offer() {
        let pool = setup_test_db().await;

        let cfd_offer = CfdOffer::from_default_with_price(Usd(dec!(10000))).unwrap();
        insert_cfd_offer(cfd_offer.clone(), &pool).await.unwrap();

        let cfd_offer_loaded = load_offer_by_id(cfd_offer.id, &pool).await.unwrap();

        assert_eq!(cfd_offer, cfd_offer_loaded);
    }

    #[tokio::test]
    async fn test_insert_and_load_cfd() {
        let pool = setup_test_db().await;

        let cfd_offer = CfdOffer::from_default_with_price(Usd(dec!(10000))).unwrap();
        let cfd = Cfd::new(
            cfd_offer.clone(),
            Usd(dec!(1000)),
            CfdState::PendingTakeRequest {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
            },
            Usd(dec!(10001)),
        )
        .unwrap();

        // the order ahs to exist in the db in order to be able to insert the cfd
        insert_cfd_offer(cfd_offer, &pool).await.unwrap();
        insert_cfd(cfd.clone(), &pool).await.unwrap();

        let cfds_from_db = load_all_cfds(&pool).await.unwrap();
        let cfd_from_db = cfds_from_db.first().unwrap().clone();
        assert_eq!(cfd, cfd_from_db)
    }

    #[tokio::test]
    async fn test_insert_new_cfd_state() {
        let pool = setup_test_db().await;

        let cfd_offer = CfdOffer::from_default_with_price(Usd(dec!(10000))).unwrap();
        let mut cfd = Cfd::new(
            cfd_offer.clone(),
            Usd(dec!(1000)),
            CfdState::PendingTakeRequest {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
            },
            Usd(dec!(10001)),
        )
        .unwrap();

        // the order ahs to exist in the db in order to be able to insert the cfd
        insert_cfd_offer(cfd_offer, &pool).await.unwrap();
        insert_cfd(cfd.clone(), &pool).await.unwrap();

        cfd.state = CfdState::Accepted {
            common: CfdStateCommon {
                transition_timestamp: SystemTime::now(),
            },
        };
        insert_new_cfd_state(cfd.clone(), &pool).await.unwrap();

        let cfds_from_db = load_all_cfds(&pool).await.unwrap();
        let cfd_from_db = cfds_from_db.first().unwrap().clone();
        assert_eq!(cfd, cfd_from_db)
    }

    async fn setup_test_db() -> SqlitePool {
        let temp_db = tempdir().unwrap().into_path().join("tempdb");

        // file has to exist in order to connect with sqlite
        File::create(temp_db.clone()).unwrap();

        dbg!(&temp_db);

        let pool = SqlitePool::connect(format!("sqlite:{}", temp_db.display()).as_str())
            .await
            .unwrap();

        do_run_migrations(&pool).await.unwrap();

        pool
    }
}
