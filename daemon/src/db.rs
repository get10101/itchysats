use anyhow::Context;
use anyhow::Result;
use chashmap_async::CHashMap;
use futures::future::BoxFuture;
use futures::FutureExt;
use futures::Stream;
use futures::StreamExt;
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
use rayon::prelude::*;
use sqlx::migrate::MigrateError;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::Connection as _;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use sqlx::Transaction;
use std::any::Any;
use std::any::TypeId;
use std::path::PathBuf;
use std::sync::Arc;
use time::Duration;

pub use closed::*;
pub use failed::*;

mod closed;
mod event_log;
mod failed;

#[derive(Clone)]
pub struct Connection {
    inner: SqlitePool,
    aggregate_cache: Arc<CHashMap<(TypeId, OrderId), Box<dyn Any + Send + Sync + 'static>>>,
}

impl Connection {
    fn new(pool: SqlitePool) -> Self {
        Self {
            inner: pool,
            aggregate_cache: Arc::new(CHashMap::new()),
        }
    }

    pub async fn close(self) {
        self.inner.close().await;
    }
}

/// Connects to the SQLite database at the given path.
///
/// If the database does not exist, it will be created. If it does exist, we load it and apply all
/// pending migrations. If applying migrations fails, the old database is backed up next to it and a
/// new one is created.
pub fn connect(path: PathBuf) -> BoxFuture<'static, Result<Connection>> {
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

                return Ok(Connection::new(pool));
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

pub async fn memory() -> Result<Connection> {
    // Note: Every :memory: database is distinct from every other. So, opening two database
    // connections each with the filename ":memory:" will create two independent in-memory
    // databases. see: https://www.sqlite.org/inmemorydb.html
    let pool = SqlitePool::connect(":memory:").await?;

    run_migrations(&pool).await?;

    Ok(Connection::new(pool))
}

async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .context("Failed to run migrations")?;

    Ok(())
}

impl Connection {
    pub async fn insert_cfd(&self, cfd: &model::Cfd) -> Result<()> {
        let mut conn = self.inner.acquire().await?;

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
        .execute(&mut conn)
        .await?;

        if query_result.rows_affected() != 1 {
            anyhow::bail!("failed to insert cfd");
        }

        Ok(())
    }

    /// Appends an event to the `events` table.
    ///
    /// To make handling of `None` events more ergonomic, you can pass anything in here that
    /// implements `Into<Option>` event.
    pub async fn append_event(&self, event: impl Into<Option<CfdEvent>>) -> Result<()> {
        let mut conn = self.inner.acquire().await?;

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
        .execute(&mut conn)
        .await?;

        if query_result.rows_affected() != 1 {
            anyhow::bail!("failed to insert event");
        }

        tracing::info!(event = %event_name, order_id = %event.id, "Appended event to database");

        Ok(())
    }

    /// Load a CFD in its latest version from the database.
    pub async fn load_open_cfd<C>(&self, id: OrderId, args: C::CtorArgs) -> Result<C, Error>
    where
        C: CfdAggregate,
    {
        let mut conn = self.inner.acquire().await?;
        let mut db_tx = conn.begin().await?;

        let cache_key = (TypeId::of::<C>(), id);
        let aggregate = std::any::type_name::<C>();

        let cfd = match self.aggregate_cache.remove(&cache_key).await {
            None => {
                // No cache entry? Load the CFD row. Version will be 0 because we haven't applied
                // any events, thus all events will be loaded.
                let cfd = load_cfd_row(&mut db_tx, id).await?;

                C::new(args, cfd)
            }
            Some(cfd) => {
                // Got a cache entry: Downcast it to the type at hand.

                *cfd.downcast::<C>()
                    .expect("we index by type id, must be able to downcast")
            }
        };
        let cfd_version = cfd.version();

        let events = load_cfd_events(&mut db_tx, id, cfd_version).await?;
        let num_events = events.len();

        tracing::debug!(order_id = %id, %aggregate, %cfd_version, %num_events, "Applying new events to CFD");

        let cfd = events.into_iter().fold(cfd, C::apply);

        self.aggregate_cache
            .insert(cache_key, Box::new(cfd.clone()))
            .await;

        db_tx.commit().await?;

        Ok(cfd)
    }

    pub fn load_all_cfds<C>(&self, args: C::CtorArgs) -> impl Stream<Item = Result<C>> + Unpin + '_
    where
        C: CfdAggregate + ClosedCfdAggregate + FailedCfdAggregate,
        C::CtorArgs: Clone + Send + Sync,
    {
        let stream = async_stream::stream! {
            let ids = self.load_open_cfd_ids().await?;
            for id in ids {
                let res = match self.load_open_cfd(id, args.clone()).await {
                    Err(Error::OpenCfdNotFound) => {
                        tracing::trace!(
                            order_id=%id,
                            target="db",
                            "Ignoring OpenCfdNotFound"
                        );
                        continue;
                    }
                    res => res.with_context(|| "Could not load open CFD {id}"),
                };

                yield res;
            }

            let ids = self.load_closed_cfd_ids().await?;
            for id in ids {
                yield self.load_closed_cfd(id, args.clone()).await
                    .with_context(|| format!("Failed to load closed CFD {id}"));
            }

            let mut conn = self.inner.acquire().await?;

            let ids = sqlx::query!(
                r#"
                SELECT
                    uuid as "uuid: model::OrderId"
                FROM
                    failed_cfds
                "#
            )
            .fetch_all(&mut *conn)
            .await?
            .into_iter()
            .map(|r| r.uuid);

            drop(conn);

            for id in ids {
                yield self.load_failed_cfd(id, args.clone()).await
                    .with_context(|| format!("Failed to load failed CFD {id}"));
            }
        };

        stream.boxed()
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
    pub fn load_all_open_cfds<C>(
        &self,
        args: C::CtorArgs,
    ) -> impl Stream<Item = Result<C>> + Unpin + '_
    where
        C: CfdAggregate + Unpin,
        C::CtorArgs: Clone + Send + Sync,
    {
        let stream = async_stream::stream! {
            let ids = self.load_open_cfd_ids().await?;

            for id in ids {
                let res = match self.load_open_cfd(id, args.clone()).await {
                    Err(Error::OpenCfdNotFound) => {
                        tracing::trace!(
                            order_id=%id,
                            target="db",
                            "Ignoring OpenCfdNotFound"
                        );
                        continue;
                    }
                    res => res.with_context(|| "Could not load open CFD {id}"),
                };

                yield res;
            }
        };

        stream.boxed()
    }

    pub async fn load_open_cfd_ids(&self) -> Result<Vec<OrderId>> {
        let mut conn = self.inner.acquire().await?;

        let ids = sqlx::query!(
            r#"
            SELECT
                uuid as "uuid: model::OrderId"
            FROM
                cfds
            "#
        )
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .map(|r| r.uuid)
        .collect();

        Ok(ids)
    }

    async fn closed_cfd_ids_according_to_the_blockchain(&self) -> Result<Vec<OrderId>> {
        let mut conn = self.inner.acquire().await?;

        let ids = sqlx::query!(
            r#"
            select
                id as cfd_id,
                uuid as "uuid: model::OrderId"
            from
                cfds
            where exists (
                select id from EVENTS as events
                where events.cfd_id = cfds.id and
                (
                    events.name = $1 or
                    events.name = $2 or
                    events.name= $3
                )
            )
            "#,
            EventKind::COLLABORATIVE_SETTLEMENT_CONFIRMED,
            EventKind::CET_CONFIRMED,
            EventKind::REFUND_CONFIRMED,
        )
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .map(|r| r.uuid)
        .collect();

        Ok(ids)
    }

    async fn failed_cfd_ids_according_to_events(&self) -> Result<Vec<OrderId>> {
        let mut conn = self.inner.acquire().await?;

        let ids = sqlx::query!(
            r#"
            select
                id as cfd_id,
                uuid as "uuid: model::OrderId"
            from
                cfds
            where exists (
                select id from EVENTS as events
                where events.cfd_id = cfds.id and
                (
                    events.name = $1 or
                    events.name = $2
                )
            )
            "#,
            model::EventKind::OFFER_REJECTED,
            model::EventKind::CONTRACT_SETUP_FAILED,
        )
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .map(|r| r.uuid)
        .collect();

        Ok(ids)
    }
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

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("The CFD requested was not found in the open CFDs")]
    OpenCfdNotFound,
    #[error("{0:#}")]
    Sqlx(#[source] sqlx::Error),
    #[error("{0:#}")]
    Other(#[source] anyhow::Error),
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Error::Sqlx(e)
    }
}

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Error::Other(e)
    }
}

/// A trait for abstracting over an aggregate.
///
/// Aggregating all available events differs based on the module. Thus, to provide a single
/// interface we ask the caller to provide us with the bare minimum API so we can build (and
/// therefore cache) the aggregate for them.
pub trait CfdAggregate: Clone + Send + Sync + 'static {
    type CtorArgs;

    fn new(args: Self::CtorArgs, cfd: Cfd) -> Self;
    fn apply(self, event: CfdEvent) -> Self;
    fn version(&self) -> u32;
}

async fn load_cfd_row(conn: &mut Transaction<'_, Sqlite>, id: OrderId) -> Result<Cfd, Error> {
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
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(Error::OpenCfdNotFound)?;

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
///
/// Events will be sorted in chronological order.
async fn load_cfd_events(
    conn: &mut Transaction<'_, Sqlite>,
    id: OrderId,
    from_version: u32,
) -> Result<Vec<CfdEvent>> {
    let mut events = sqlx::query!(
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
    .into_par_iter()
    .map(|row| {
        Ok(CfdEvent {
            timestamp: row.created_at,
            id,
            event: EventKind::from_json(row.name, row.data)?,
        })
    })
    .collect::<Result<Vec<_>>>()?;

    events.sort_unstable_by(CfdEvent::chronologically);

    Ok(events)
}

async fn delete_from_cfds_table(conn: &mut Transaction<'_, Sqlite>, id: OrderId) -> Result<()> {
    let query_result = sqlx::query!(
        r#"
        DELETE FROM
            cfds
        WHERE
            cfds.uuid = $1
        "#,
        id,
    )
    .execute(&mut *conn)
    .await?;

    if query_result.rows_affected() != 1 {
        anyhow::bail!("failed to delete from cfds");
    }

    Ok(())
}

async fn delete_from_events_table(conn: &mut Transaction<'_, Sqlite>, id: OrderId) -> Result<()> {
    let query_result = sqlx::query!(
        r#"
        DELETE FROM
            events
        WHERE events.cfd_id IN
            (SELECT id FROM cfds WHERE cfds.uuid = $1)
        "#,
        id,
    )
    .execute(&mut *conn)
    .await?;

    if query_result.rows_affected() < 1 {
        anyhow::bail!("failed to delete from events");
    }

    Ok(())
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

    #[tokio::test]
    async fn test_insert_and_load_cfd() {
        let db = memory().await.unwrap();

        let cfd = dummy_cfd();
        db.insert_cfd(&cfd).await.unwrap();
        let mut conn = db.inner.acquire().await.unwrap();
        let mut db_tx = conn.begin().await.unwrap();

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
        } = load_cfd_row(&mut db_tx, cfd.id()).await.unwrap();

        db_tx.commit().await.unwrap();

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
    async fn test_append_events() {
        let db = memory().await.unwrap();

        let cfd = dummy_cfd();
        db.insert_cfd(&cfd).await.unwrap();

        let timestamp = Timestamp::now();

        let event1 = CfdEvent {
            timestamp,
            id: cfd.id(),
            event: EventKind::OfferRejected,
        };

        db.append_event(event1.clone()).await.unwrap();

        let mut conn = db.inner.acquire().await.unwrap();

        let mut db_tx = conn.begin().await.unwrap();
        let events = load_cfd_events(&mut db_tx, cfd.id(), 0).await.unwrap();
        db_tx.commit().await.unwrap();
        assert_eq!(events, vec![event1.clone()]);

        let event2 = CfdEvent {
            timestamp,
            id: cfd.id(),
            event: EventKind::RevokeConfirmed,
        };

        db.append_event(event2.clone()).await.unwrap();

        // let mut conn = db.inner.acquire().await.unwrap();
        let mut db_tx = conn.begin().await.unwrap();
        let events = load_cfd_events(&mut db_tx, cfd.id(), 0).await.unwrap();
        db_tx.commit().await.unwrap();
        assert_eq!(events, vec![event1, event2])
    }

    pub fn dummy_cfd() -> Cfd {
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

    pub fn lock_confirmed(cfd: &Cfd) -> CfdEvent {
        CfdEvent {
            timestamp: Timestamp::now(),
            id: cfd.id(),
            event: EventKind::LockConfirmed,
        }
    }

    pub fn setup_failed(cfd: &Cfd) -> CfdEvent {
        CfdEvent {
            timestamp: Timestamp::now(),
            id: cfd.id(),
            event: EventKind::ContractSetupFailed,
        }
    }

    pub fn order_rejected(cfd: &Cfd) -> CfdEvent {
        CfdEvent {
            timestamp: Timestamp::now(),
            id: cfd.id(),
            event: EventKind::OfferRejected,
        }
    }
}
