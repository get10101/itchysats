use crate::models;
use crate::Connection;
use anyhow::Result;
use model::Identity;
use time::OffsetDateTime;

impl Connection {
    /// Record the time at which we hear about a `taker_id` for the
    /// first time.
    ///
    /// If we have already heard about the `taker_id`, we keep the
    /// original `timestamp`.
    pub async fn try_insert_first_seen(
        &self,
        taker_id: Identity,
        timestamp: OffsetDateTime,
    ) -> Result<()> {
        let mut conn = self.inner.acquire().await?;
        let taker_id = models::Identity::from(taker_id);

        let timestamp = timestamp.unix_timestamp();

        sqlx::query!(
            r#"
            INSERT OR IGNORE INTO time_to_first_position
            (
                taker_id,
                first_seen_timestamp
            )
            VALUES ($1, $2)
            "#,
            taker_id,
            timestamp,
        )
        .execute(&mut *conn)
        .await?;

        Ok(())
    }

    /// Record the time at which we hear about a taker with `taker_id`
    /// opening a position for the first time.
    ///
    /// If we have already heard about the taker opening a position
    /// before, we keep the original `timestamp`.
    pub async fn try_insert_first_position(
        &self,
        taker_id: Identity,
        timestamp: OffsetDateTime,
    ) -> Result<()> {
        let mut conn = self.inner.acquire().await?;

        let timestamp = timestamp.unix_timestamp();
        let taker_id = models::Identity::from(taker_id);

        sqlx::query!(
            r#"
            UPDATE time_to_first_position
            SET first_position_timestamp = $2
            WHERE taker_id = $1 and first_position_timestamp is NULL
            "#,
            taker_id,
            timestamp,
        )
        .execute(&mut *conn)
        .await?;

        Ok(())
    }
}

// We cannot hide this under the `test` compilation flag because it
// makes it much less convenient to call `cargo sqlx prepare`.
#[allow(dead_code)]
mod sqlx_test_utils {
    use super::*;
    use sqlx::SqliteConnection;

    pub(crate) async fn load_first_seen_timestamp(
        conn: &mut SqliteConnection,
        taker_id: Identity,
    ) -> Result<Option<OffsetDateTime>> {
        let taker_id = models::Identity::from(taker_id);

        let row = sqlx::query!(
            r#"
            SELECT
                first_seen_timestamp
            FROM
                time_to_first_position
            WHERE
                taker_id = $1
            "#,
            taker_id
        )
        .fetch_optional(&mut *conn)
        .await?;

        let timestamp = row
            .map(|row| {
                row.first_seen_timestamp
                    .map(OffsetDateTime::from_unix_timestamp)
            })
            .unwrap_or(None)
            .transpose()?;

        Ok(timestamp)
    }

    pub(crate) async fn load_first_position_timestamp(
        conn: &mut SqliteConnection,
        taker_id: Identity,
    ) -> Result<Option<OffsetDateTime>> {
        let taker_id = models::Identity::from(taker_id);

        let row = sqlx::query!(
            r#"
            SELECT
                first_position_timestamp
            FROM
                time_to_first_position
            WHERE
                taker_id = $1
            "#,
            taker_id
        )
        .fetch_optional(&mut *conn)
        .await?;

        let timestamp = row
            .map(|row| {
                row.first_position_timestamp
                    .map(OffsetDateTime::from_unix_timestamp)
            })
            .unwrap_or(None)
            .transpose()?;

        Ok(timestamp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory;
    use sqlx_test_utils::load_first_position_timestamp;
    use sqlx_test_utils::load_first_seen_timestamp;

    #[tokio::test]
    async fn given_inserted_first_seen_when_trying_to_insert_second_seen_then_timestamp_does_not_change(
    ) {
        let db = memory().await.unwrap();
        let mut conn = db.inner.acquire().await.unwrap();

        let taker_id = dummy_identity();

        let first_inserted_timestamp = OffsetDateTime::from_unix_timestamp(1).unwrap();
        db.try_insert_first_seen(taker_id, first_inserted_timestamp)
            .await
            .unwrap();

        let first_loaded_timestamp = load_first_seen_timestamp(&mut *conn, taker_id)
            .await
            .unwrap();

        let second_inserted_timestamp = OffsetDateTime::from_unix_timestamp(2).unwrap();
        db.try_insert_first_seen(taker_id, second_inserted_timestamp)
            .await
            .unwrap();

        let second_loaded_timestamp = load_first_seen_timestamp(&mut *conn, taker_id)
            .await
            .unwrap();

        assert_eq!(Some(first_inserted_timestamp), first_loaded_timestamp);
        assert_eq!(Some(first_inserted_timestamp), second_loaded_timestamp);

        assert_ne!(Some(second_inserted_timestamp), second_loaded_timestamp);
    }

    #[tokio::test]
    async fn given_inserted_first_position_when_trying_to_insert_second_position_then_timestamp_does_not_change(
    ) {
        let db = memory().await.unwrap();
        let mut conn = db.inner.acquire().await.unwrap();

        let taker_id = dummy_identity();

        db.try_insert_first_seen(taker_id, OffsetDateTime::from_unix_timestamp(0).unwrap())
            .await
            .unwrap();

        let first_inserted_timestamp = OffsetDateTime::from_unix_timestamp(1).unwrap();
        db.try_insert_first_position(taker_id, first_inserted_timestamp)
            .await
            .unwrap();

        let first_loaded_timestamp = load_first_position_timestamp(&mut *conn, taker_id)
            .await
            .unwrap();

        let second_inserted_timestamp = OffsetDateTime::from_unix_timestamp(2).unwrap();
        db.try_insert_first_position(taker_id, second_inserted_timestamp)
            .await
            .unwrap();

        let second_loaded_timestamp = load_first_position_timestamp(&mut *conn, taker_id)
            .await
            .unwrap();

        assert_eq!(Some(first_inserted_timestamp), first_loaded_timestamp);
        assert_eq!(Some(first_inserted_timestamp), second_loaded_timestamp);

        assert_ne!(Some(second_inserted_timestamp), second_loaded_timestamp);
    }

    fn dummy_identity() -> Identity {
        Identity::new(x25519_dalek::PublicKey::from(
            *b"hello world, oh what a beautiful",
        ))
    }
}
