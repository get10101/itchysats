use crate::model;
use crate::model::cfd::CfdEvent;
use crate::model::cfd::Event;
use crate::model::cfd::OrderId;
use crate::model::cfd::Role;
use crate::model::FundingRate;
use crate::model::Identity;
use crate::model::Leverage;
use crate::model::OpeningFee;
use crate::model::Position;
use crate::model::Price;
use crate::model::TxFeeRate;
use crate::model::Usd;
use anyhow::Context;
use anyhow::Result;
use futures::future::BoxFuture;
use futures::FutureExt;
use sqlx::migrate::MigrateError;
use sqlx::pool::PoolConnection;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use std::path::PathBuf;
use time::Duration;

/// Connects to the SQLite database at the given path.
///
/// If the database does not exist, it will be created. If it does exist, we load it and apply all
/// pending migrations. If applying migrations fails, the old database is backed up next to it and a
/// new one is created.
pub fn connect(path: PathBuf) -> BoxFuture<'static, Result<SqlitePool>> {
    async move {
        let pool = SqlitePool::connect_with(
            SqliteConnectOptions::new()
                .create_if_missing(true)
                .filename(&path),
        )
        .await?;

        let path_display = path.display();

        // Attempt to migrate, early return if successful
        let error = match run_migrations(&pool).await {
            Ok(()) => {
                tracing::info!("Opened database at {path_display}");

                return Ok(pool);
            }
            Err(e) => e,
        };

        // Attempt to recover from _some_ problems during migration.
        // These two can happen if someone tampered with the migrations or messed with the DB.
        if let Some(MigrateError::VersionMissing(_) | MigrateError::VersionMismatch(_)) =
            error.downcast_ref::<MigrateError>()
        {
            tracing::error!("{:#}", error);

            let unix_timestamp = time::OffsetDateTime::now_utc().unix_timestamp();

            let new_path = PathBuf::from(format!("{path_display}-{unix_timestamp}-backup"));
            let new_path_display = new_path.display();

            tracing::info!("Backing up old database at {path_display} to {new_path_display}");

            tokio::fs::rename(&path, &new_path)
                .await
                .context("Failed to rename database file")?;

            tracing::info!("Starting with a new database!");

            // recurse to reconnect (async recursion requires a `BoxFuture`)
            return connect(path).await;
        }

        Err(error)
    }
    .boxed()
}

pub async fn memory() -> Result<SqlitePool> {
    // Note: Every :memory: database is distinct from every other. So, opening two database
    // connections each with the filename ":memory:" will create two independent in-memory
    // databases. see: https://www.sqlite.org/inmemorydb.html
    let pool = SqlitePool::connect(":memory:").await?;

    run_migrations(&pool).await?;

    Ok(pool)
}

async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .context("Failed to run migrations")?;

    Ok(())
}

pub async fn insert_cfd(cfd: &model::cfd::Cfd, conn: &mut PoolConnection<Sqlite>) -> Result<()> {
    let query_result = sqlx::query(
        r#"
        insert into cfds (
            uuid,
            position,
            initial_price,
            leverage,
            settlement_time_interval_hours,
            quantity_usd,
            counterparty_network_identity,
            role,
            opening_fee,
            initial_funding_rate,
            initial_tx_fee_rate
        ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
    )
    .bind(&cfd.id())
    .bind(&cfd.position())
    .bind(&cfd.initial_price())
    .bind(&cfd.leverage())
    .bind(&cfd.settlement_time_interval_hours().whole_hours())
    .bind(&cfd.quantity())
    .bind(&cfd.counterparty_network_identity())
    .bind(&cfd.role())
    .bind(&cfd.opening_fee())
    .bind(&cfd.initial_funding_rate())
    .bind(&cfd.initial_tx_fee_rate())
    .execute(conn)
    .await?;

    if query_result.rows_affected() != 1 {
        anyhow::bail!("failed to insert cfd");
    }

    Ok(())
}

/// Appends an event to the `events` table.
///
/// To make handling of `None` events more ergonomic, you can pass anything in here that implements
/// `Into<Option>` event.
pub async fn append_event(
    event: impl Into<Option<Event>>,
    conn: &mut PoolConnection<Sqlite>,
) -> Result<()> {
    let event = match event.into() {
        Some(event) => event,
        None => return Ok(()),
    };

    let (event_name, event_data) = event.event.to_json();

    let query_result = sqlx::query(
        r##"
        insert into events (
            cfd_id,
            name,
            data,
            created_at
        ) values (
            (select id from cfds where cfds.uuid = $1),
            $2, $3, $4
        )"##,
    )
    .bind(&event.id)
    .bind(&event_name)
    .bind(&event_data)
    .bind(&event.timestamp)
    .execute(conn)
    .await?;

    if query_result.rows_affected() != 1 {
        anyhow::bail!("failed to insert event");
    }

    Ok(())
}

// TODO: Make sqlx directly instantiate this struct instead of mapping manually. Need to create
// newtype for `settlement_interval`.
pub struct Cfd {
    pub id: OrderId,
    pub position: Position,
    pub initial_price: Price,
    pub leverage: Leverage,
    pub settlement_interval: Duration,
    pub quantity_usd: Usd,
    pub counterparty_network_identity: Identity,
    pub role: Role,
    pub opening_fee: OpeningFee,
    pub initial_funding_rate: FundingRate,
    pub initial_tx_fee_rate: TxFeeRate,
}

pub async fn load_cfd(id: OrderId, conn: &mut PoolConnection<Sqlite>) -> Result<(Cfd, Vec<Event>)> {
    let cfd_row = sqlx::query!(
        r#"
            select
                id as cfd_id,
                uuid as "uuid: crate::model::cfd::OrderId",
                position as "position: crate::model::Position",
                initial_price as "initial_price: crate::model::Price",
                leverage as "leverage: crate::model::Leverage",
                settlement_time_interval_hours,
                quantity_usd as "quantity_usd: crate::model::Usd",
                counterparty_network_identity as "counterparty_network_identity: crate::model::Identity",
                role as "role: crate::model::cfd::Role",
                opening_fee as "opening_fee: crate::model::OpeningFee",
                initial_funding_rate as "initial_funding_rate: crate::model::FundingRate",
                initial_tx_fee_rate as "initial_tx_fee_rate: crate::model::TxFeeRate"
            from
                cfds
            where
                cfds.uuid = $1
            "#,
            id
    )
    .fetch_one(&mut *conn)
    .await?;

    let cfd = Cfd {
        id: cfd_row.uuid,
        position: cfd_row.position,
        initial_price: cfd_row.initial_price,
        leverage: cfd_row.leverage,
        settlement_interval: Duration::hours(cfd_row.settlement_time_interval_hours),
        quantity_usd: cfd_row.quantity_usd,
        counterparty_network_identity: cfd_row.counterparty_network_identity,
        role: cfd_row.role,
        opening_fee: cfd_row.opening_fee,
        initial_funding_rate: cfd_row.initial_funding_rate,
        initial_tx_fee_rate: cfd_row.initial_tx_fee_rate,
    };

    let events = sqlx::query!(
        r#"

        select
            name,
            data,
            created_at as "created_at: crate::model::Timestamp"
        from
            events
        where
            cfd_id = $1
            "#,
        cfd_row.cfd_id
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(Event {
            timestamp: row.created_at,
            id,
            event: CfdEvent::from_json(row.name, row.data)?,
        })
    })
    .collect::<Result<Vec<_>>>()?;

    Ok((cfd, events))
}

pub async fn load_all_cfd_ids(conn: &mut PoolConnection<Sqlite>) -> Result<Vec<OrderId>> {
    let ids = sqlx::query!(
        r#"
            select
                id as cfd_id,
                uuid as "uuid: crate::model::cfd::OrderId"
            from
                cfds
            order by cfd_id desc
            "#
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|r| r.uuid)
    .collect();

    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::cfd::Cfd;
    use crate::model::cfd::Role;
    use crate::model::Leverage;
    use crate::model::Position;
    use crate::model::Price;
    use crate::model::Timestamp;
    use crate::model::TxFeeRate;
    use crate::model::Usd;
    use bdk::bitcoin::Amount;
    use pretty_assertions::assert_eq;
    use rust_decimal_macros::dec;
    use sqlx::SqlitePool;

    #[tokio::test]
    async fn test_insert_and_load_cfd() {
        let mut conn = setup_test_db().await;

        let cfd = Cfd::dummy().insert(&mut conn).await;
        let (
            super::Cfd {
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
            },
            _,
        ) = load_cfd(cfd.id(), &mut conn).await.unwrap();

        assert_eq!(cfd.id(), id);
        assert_eq!(cfd.position(), position);
        assert_eq!(cfd.initial_price(), initial_price);
        assert_eq!(cfd.leverage(), leverage);
        assert_eq!(cfd.settlement_time_interval_hours(), settlement_interval);
        assert_eq!(cfd.quantity(), quantity_usd);
        assert_eq!(
            cfd.counterparty_network_identity(),
            counterparty_network_identity
        );
        assert_eq!(cfd.role(), role);
        assert_eq!(cfd.opening_fee(), opening_fee);
        assert_eq!(cfd.initial_funding_rate(), initial_funding_rate);
        assert_eq!(cfd.initial_tx_fee_rate(), initial_tx_fee_rate);
    }

    #[tokio::test]
    async fn test_insert_and_load_cfd_ids_order_desc() {
        let mut conn = setup_test_db().await;

        let cfd_1 = Cfd::dummy().insert(&mut conn).await;
        let cfd_2 = Cfd::dummy().insert(&mut conn).await;
        let cfd_3 = Cfd::dummy().insert(&mut conn).await;

        let ids = load_all_cfd_ids(&mut conn).await.unwrap();

        assert_eq!(vec![cfd_3.id(), cfd_2.id(), cfd_1.id()], ids)
    }

    #[tokio::test]
    async fn test_append_events() {
        let mut conn = setup_test_db().await;

        let cfd = Cfd::dummy().insert(&mut conn).await;

        let timestamp = Timestamp::now();

        let event1 = Event {
            timestamp,
            id: cfd.id(),
            event: CfdEvent::OfferRejected,
        };

        append_event(event1.clone(), &mut conn).await.unwrap();
        let (_, events) = load_cfd(cfd.id(), &mut conn).await.unwrap();
        assert_eq!(events, vec![event1.clone()]);

        let event2 = Event {
            timestamp,
            id: cfd.id(),
            event: CfdEvent::RevokeConfirmed,
        };

        append_event(event2.clone(), &mut conn).await.unwrap();
        let (_, events) = load_cfd(cfd.id(), &mut conn).await.unwrap();
        assert_eq!(events, vec![event1, event2])
    }

    async fn setup_test_db() -> PoolConnection<Sqlite> {
        let pool = SqlitePool::connect(":memory:").await.unwrap();

        run_migrations(&pool).await.unwrap();

        pool.acquire().await.unwrap()
    }

    impl Cfd {
        fn dummy() -> Self {
            Self::new(
                OrderId::default(),
                Position::Long,
                Price::new(dec!(60_000)).unwrap(),
                Leverage::new(2).unwrap(),
                Duration::hours(24),
                Role::Taker,
                Usd::new(dec!(1_000)),
                "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"
                    .parse()
                    .unwrap(),
                OpeningFee::new(Amount::from_sat(2000)),
                FundingRate::default(),
                TxFeeRate::default(),
            )
        }

        /// Insert this [`Cfd`] into the database, returning the instance for further chaining.
        async fn insert(self, conn: &mut PoolConnection<Sqlite>) -> Self {
            insert_cfd(&self, conn).await.unwrap();

            self
        }
    }
}
