use anyhow::Context;
use anyhow::Result;
use futures::future::BoxFuture;
use futures::FutureExt;
use model::CfdEvent;
use model::EventKind;
use model::FundingRate;
use model::Identity;
use model::Leverage;
use model::OpeningFee;
use model::OrderId;
use model::Position;
use model::Price;
use model::Role;
use model::TxFeeRate;
use model::Usd;
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

pub async fn insert_cfd(cfd: &model::Cfd, conn: &mut PoolConnection<Sqlite>) -> Result<()> {
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
    .bind(&cfd.taker_leverage())
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
    event: impl Into<Option<CfdEvent>>,
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

    tracing::info!(event = %event_name, order_id = %event.id, "Appended event to database");

    Ok(())
}

// TODO: Make sqlx directly instantiate this struct instead of mapping manually. Need to create
// newtype for `settlement_interval`.
#[derive(Clone, Copy)]
pub struct Cfd {
    pub id: OrderId,
    pub position: Position,
    pub initial_price: Price,
    pub taker_leverage: Leverage,
    pub settlement_interval: Duration,
    pub quantity_usd: Usd,
    pub counterparty_network_identity: Identity,
    pub role: Role,
    pub opening_fee: OpeningFee,
    pub initial_funding_rate: FundingRate,
    pub initial_tx_fee_rate: TxFeeRate,
}

/// A trait for abstracting over an aggregate.
///
/// Aggregating all available events differs based on the module. Thus, to provide a single
/// interface we ask the caller to provide us with the bare minimum API so we can build the
/// aggregate for them.
pub trait CfdAggregate: Clone + Send + Sync + 'static {
    type CtorArgs;

    fn new(args: Self::CtorArgs, cfd: Cfd) -> Self;
    fn apply(self, event: CfdEvent) -> Self;
    fn version(&self) -> u32;
}

/// Load a CFD in its latest version from the database.
pub async fn load_cfd<C>(
    id: OrderId,
    conn: &mut PoolConnection<Sqlite>,
    args: C::CtorArgs,
) -> Result<C>
where
    C: CfdAggregate,
{
    let row = load_cfd_row(id, conn).await?;
    let cfd = C::new(args, row);

    let cfd = load_cfd_events(id, conn, 0)
        .await?
        .into_iter()
        .fold(cfd, C::apply);

    Ok(cfd)
}

async fn load_cfd_row(id: OrderId, conn: &mut PoolConnection<Sqlite>) -> Result<Cfd> {
    let cfd_row = sqlx::query!(
        r#"
            select
                id as cfd_id,
                uuid as "uuid: model::OrderId",
                position as "position: model::Position",
                initial_price as "initial_price: model::Price",
                leverage as "leverage: model::Leverage",
                settlement_time_interval_hours,
                quantity_usd as "quantity_usd: model::Usd",
                counterparty_network_identity as "counterparty_network_identity: model::Identity",
                role as "role: model::Role",
                opening_fee as "opening_fee: model::OpeningFee",
                initial_funding_rate as "initial_funding_rate: model::FundingRate",
                initial_tx_fee_rate as "initial_tx_fee_rate: model::TxFeeRate"
            from
                cfds
            where
                cfds.uuid = $1
            "#,
        id
    )
    .fetch_one(&mut *conn)
    .await?;

    Ok(Cfd {
        id: cfd_row.uuid,
        position: cfd_row.position,
        initial_price: cfd_row.initial_price,
        taker_leverage: cfd_row.leverage,
        settlement_interval: Duration::hours(cfd_row.settlement_time_interval_hours),
        quantity_usd: cfd_row.quantity_usd,
        counterparty_network_identity: cfd_row.counterparty_network_identity,
        role: cfd_row.role,
        opening_fee: cfd_row.opening_fee,
        initial_funding_rate: cfd_row.initial_funding_rate,
        initial_tx_fee_rate: cfd_row.initial_tx_fee_rate,
    })
}

/// Load events for a given CFD but only onwards from the specified version.
///
/// The version of a CFD is the number of events that have been applied. If we have an aggregate
/// instance in version 3, we can avoid loading the first 3 events and only apply the ones after.
async fn load_cfd_events(
    id: OrderId,
    conn: &mut PoolConnection<Sqlite>,
    from_version: u32,
) -> Result<Vec<CfdEvent>> {
    let events = sqlx::query!(
        r#"

        select
            name,
            data,
            created_at as "created_at: model::Timestamp"
        from
            events
        join
            cfds c on c.id = events.cfd_id
        where
            uuid = $1
        limit $2,-1
            "#,
        id,
        from_version
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok(CfdEvent {
            timestamp: row.created_at,
            id,
            event: EventKind::from_json(row.name, row.data)?,
        })
    })
    .collect::<Result<Vec<_>>>()?;

    Ok(events)
}

pub async fn load_all_cfd_ids(conn: &mut PoolConnection<Sqlite>) -> Result<Vec<OrderId>> {
    let ids = sqlx::query!(
        r#"
            select
                id as cfd_id,
                uuid as "uuid: model::OrderId"
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

/// Loads all CFDs where we are still able to append events
///
/// This function is to be called when we only want to process CFDs where events can still be
/// appended, but ignore all other CFDs.
/// Open in this context means that the CFD is not final yet, i.e. we can still append events.
/// In this context a CFD is not open anymore if one of the following happened:
/// 1. Event of the confirmation of a payout (spend) transaction on the blockchain was recorded
///     Cases: Collaborative settlement, CET, Refund
/// 2. Event that fails the CFD early was recorded, meaning it becomes irrelevant for processing
///     Cases: Setup failed, Taker's take order rejected
pub async fn load_open_cfd_ids(conn: &mut PoolConnection<Sqlite>) -> Result<Vec<OrderId>> {
    let ids = sqlx::query!(
        r#"
            select
                id as cfd_id,
                uuid as "uuid: model::OrderId"
            from
                cfds
            where not exists (
                select id from EVENTS as events
                where cfd_id = cfds.id and
                (
                    events.name = $1 or
                    events.name = $2 or
                    events.name= $3 or
                    events.name= $4 or
                    events.name= $5
                )
            )
            order by cfd_id desc
            "#,
        EventKind::COLLABORATIVE_SETTLEMENT_CONFIRMED,
        EventKind::CET_CONFIRMED,
        EventKind::REFUND_CONFIRMED,
        EventKind::CONTRACT_SETUP_FAILED,
        EventKind::OFFER_REJECTED
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
    use bdk::bitcoin::Amount;
    use model::Cfd;
    use model::Leverage;
    use model::OpeningFee;
    use model::Position;
    use model::Price;
    use model::Role;
    use model::Timestamp;
    use model::TxFeeRate;
    use model::Usd;
    use pretty_assertions::assert_eq;
    use rust_decimal_macros::dec;
    use sqlx::SqlitePool;

    #[tokio::test]
    async fn test_insert_and_load_cfd() {
        let mut conn = setup_test_db().await;

        let cfd = insert(dummy_cfd(), &mut conn).await;
        let super::Cfd {
            id,
            position,
            initial_price,
            taker_leverage: leverage,
            settlement_interval,
            quantity_usd,
            counterparty_network_identity,
            role,
            opening_fee,
            initial_funding_rate,
            initial_tx_fee_rate,
        } = load_cfd_row(cfd.id(), &mut conn).await.unwrap();

        assert_eq!(cfd.id(), id);
        assert_eq!(cfd.position(), position);
        assert_eq!(cfd.initial_price(), initial_price);
        assert_eq!(cfd.taker_leverage(), leverage);
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

        let cfd_1 = insert(dummy_cfd(), &mut conn).await;
        let cfd_2 = insert(dummy_cfd(), &mut conn).await;
        let cfd_3 = insert(dummy_cfd(), &mut conn).await;

        let ids = load_all_cfd_ids(&mut conn).await.unwrap();

        assert_eq!(vec![cfd_3.id(), cfd_2.id(), cfd_1.id()], ids)
    }

    #[tokio::test]
    async fn test_append_events() {
        let mut conn = setup_test_db().await;

        let cfd = insert(dummy_cfd(), &mut conn).await;

        let timestamp = Timestamp::now();

        let event1 = CfdEvent {
            timestamp,
            id: cfd.id(),
            event: EventKind::OfferRejected,
        };

        append_event(event1.clone(), &mut conn).await.unwrap();
        let events = load_cfd_events(cfd.id(), &mut conn, 0).await.unwrap();
        assert_eq!(events, vec![event1.clone()]);

        let event2 = CfdEvent {
            timestamp,
            id: cfd.id(),
            event: EventKind::RevokeConfirmed,
        };

        append_event(event2.clone(), &mut conn).await.unwrap();
        let events = load_cfd_events(cfd.id(), &mut conn, 0).await.unwrap();
        assert_eq!(events, vec![event1, event2])
    }

    #[tokio::test]
    async fn given_collaborative_close_confirmed_then_do_not_load_non_final_cfd() {
        let mut conn = setup_test_db().await;

        let cfd_final = insert(dummy_cfd(), &mut conn).await;
        append_event(lock_confirmed(&cfd_final), &mut conn)
            .await
            .unwrap();
        append_event(collab_settlement_confirmed(&cfd_final), &mut conn)
            .await
            .unwrap();

        let cfd_ids = load_open_cfd_ids(&mut conn).await.unwrap();

        assert!(cfd_ids.is_empty());
    }

    #[tokio::test]
    async fn given_cet_confirmed_then_do_not_load_non_final_cfd() {
        let mut conn = setup_test_db().await;

        let cfd_final = insert(dummy_cfd(), &mut conn).await;
        append_event(lock_confirmed(&cfd_final), &mut conn)
            .await
            .unwrap();
        append_event(cet_confirmed(&cfd_final), &mut conn)
            .await
            .unwrap();

        let cfd_ids = load_open_cfd_ids(&mut conn).await.unwrap();
        assert!(cfd_ids.is_empty());
    }

    #[tokio::test]
    async fn given_refund_confirmed_then_do_not_load_non_final_cfd() {
        let mut conn = setup_test_db().await;

        let cfd_final = insert(dummy_cfd(), &mut conn).await;
        append_event(lock_confirmed(&cfd_final), &mut conn)
            .await
            .unwrap();
        append_event(refund_confirmed(&cfd_final), &mut conn)
            .await
            .unwrap();

        let cfd_ids = load_open_cfd_ids(&mut conn).await.unwrap();
        assert!(cfd_ids.is_empty());
    }

    #[tokio::test]
    async fn given_setup_failed_then_do_not_load_non_final_cfd() {
        let mut conn = setup_test_db().await;

        let cfd_final = insert(dummy_cfd(), &mut conn).await;
        append_event(lock_confirmed(&cfd_final), &mut conn)
            .await
            .unwrap();
        append_event(setup_failed(&cfd_final), &mut conn)
            .await
            .unwrap();

        let cfd_ids = load_open_cfd_ids(&mut conn).await.unwrap();
        assert!(cfd_ids.is_empty());
    }

    #[tokio::test]
    async fn given_order_rejected_then_do_not_load_non_final_cfd() {
        let mut conn = setup_test_db().await;

        let cfd_final = insert(dummy_cfd(), &mut conn).await;
        append_event(lock_confirmed(&cfd_final), &mut conn)
            .await
            .unwrap();
        append_event(order_rejected(&cfd_final), &mut conn)
            .await
            .unwrap();

        let cfd_ids = load_open_cfd_ids(&mut conn).await.unwrap();
        assert!(cfd_ids.is_empty());
    }

    #[tokio::test]
    async fn given_final_and_non_final_cfd_then_non_final_one_still_loaded() {
        let mut conn = setup_test_db().await;

        let cfd_not_final = insert(dummy_cfd(), &mut conn).await;
        append_event(lock_confirmed(&cfd_not_final), &mut conn)
            .await
            .unwrap();

        let cfd_final = insert(dummy_cfd(), &mut conn).await;
        append_event(lock_confirmed(&cfd_final), &mut conn)
            .await
            .unwrap();
        append_event(order_rejected(&cfd_final), &mut conn)
            .await
            .unwrap();

        let cfd_ids = load_open_cfd_ids(&mut conn).await.unwrap();

        assert_eq!(cfd_ids.len(), 1);
        assert_eq!(*cfd_ids.first().unwrap(), cfd_not_final.id())
    }

    async fn setup_test_db() -> PoolConnection<Sqlite> {
        let pool = SqlitePool::connect(":memory:").await.unwrap();

        run_migrations(&pool).await.unwrap();

        pool.acquire().await.unwrap()
    }

    fn dummy_cfd() -> Cfd {
        Cfd::new(
            OrderId::default(),
            Position::Long,
            Price::new(dec!(60_000)).unwrap(),
            Leverage::TWO,
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

    /// Insert this [`Cfd`] into the database, returning the instance
    /// for further chaining.
    pub async fn insert(cfd: Cfd, conn: &mut PoolConnection<Sqlite>) -> Cfd {
        insert_cfd(&cfd, conn).await.unwrap();
        cfd
    }

    fn lock_confirmed(cfd: &Cfd) -> CfdEvent {
        CfdEvent {
            timestamp: Timestamp::now(),
            id: cfd.id(),
            event: EventKind::LockConfirmed,
        }
    }

    fn collab_settlement_confirmed(cfd: &Cfd) -> CfdEvent {
        CfdEvent {
            timestamp: Timestamp::now(),
            id: cfd.id(),
            event: EventKind::CollaborativeSettlementConfirmed,
        }
    }

    fn cet_confirmed(cfd: &Cfd) -> CfdEvent {
        CfdEvent {
            timestamp: Timestamp::now(),
            id: cfd.id(),
            event: EventKind::CetConfirmed,
        }
    }

    fn refund_confirmed(cfd: &Cfd) -> CfdEvent {
        CfdEvent {
            timestamp: Timestamp::now(),
            id: cfd.id(),
            event: EventKind::RefundConfirmed,
        }
    }

    fn setup_failed(cfd: &Cfd) -> CfdEvent {
        CfdEvent {
            timestamp: Timestamp::now(),
            id: cfd.id(),
            event: EventKind::ContractSetupFailed,
        }
    }

    fn order_rejected(cfd: &Cfd) -> CfdEvent {
        CfdEvent {
            timestamp: Timestamp::now(),
            id: cfd.id(),
            event: EventKind::OfferRejected,
        }
    }
}
