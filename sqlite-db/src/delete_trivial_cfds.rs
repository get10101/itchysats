use crate::Connection;
use anyhow::Result;

impl Connection {
    /// Delete all CFDs which have no events associated with them.
    ///
    /// It is recommended to call this method right after calling
    /// `sqlite_db::connect`, before new CFDs are added to the
    /// database. This is to ensure that no relevant data is deleted.
    pub async fn delete_trivial_cfds(&self) -> Result<()> {
        let mut conn = self.inner.acquire().await?;

        // FIXME: I cannot run `cargo sqlx prepare` locally :(, so I
        // can't add/modify any of the `sqlx` macros.
        let affected_rows = sqlx::query(
            r#"
            DELETE FROM
                cfds
            WHERE
                id
            NOT IN
            (
                SELECT
                    events.cfd_id
                FROM
                    events
            )
            "#,
        )
        .execute(&mut *conn)
        .await?
        .rows_affected();

        if affected_rows > 0 {
            tracing::info!(n = %affected_rows, "Deleted trivial CFDs");
        } else {
            tracing::info!("No trivial CFDs to delete");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use model::CfdEvent;
    use model::EventKind;

    use crate::memory;
    use crate::tests::dummy_cfd;

    #[tokio::test]
    async fn given_trivial_cfds_then_they_are_deleted_from_cfds() {
        let db = memory().await.unwrap();

        let cfd_0 = dummy_cfd();
        db.insert_cfd(&cfd_0).await.unwrap();

        let cfd_1 = dummy_cfd();
        db.insert_cfd(&cfd_1).await.unwrap();

        let ids_before = db.load_open_cfd_ids().await.unwrap();

        db.delete_trivial_cfds().await.unwrap();

        let ids_after = db.load_open_cfd_ids().await.unwrap();

        assert!(ids_before.contains(&cfd_0.id()));
        assert!(ids_before.contains(&cfd_1.id()));

        assert!(ids_after.is_empty())
    }

    #[tokio::test]
    async fn given_trivial_cfd_among_others_then_trivial_cfd_is_only_one_deleted() {
        let db = memory().await.unwrap();

        let trivial_cfd = dummy_cfd();
        db.insert_cfd(&trivial_cfd).await.unwrap();

        let cfd_0 = dummy_cfd();
        db.insert_cfd(&cfd_0).await.unwrap();
        db.append_event(CfdEvent::new(cfd_0.id(), EventKind::ContractSetupStarted))
            .await
            .unwrap();

        let cfd_1 = dummy_cfd();
        db.insert_cfd(&cfd_1).await.unwrap();
        db.append_event(CfdEvent::new(cfd_1.id(), EventKind::ContractSetupStarted))
            .await
            .unwrap();

        let ids_before = db.load_open_cfd_ids().await.unwrap();

        db.delete_trivial_cfds().await.unwrap();

        let ids_after = db.load_open_cfd_ids().await.unwrap();

        assert!(ids_before.contains(&trivial_cfd.id()));
        assert!(ids_before.contains(&cfd_0.id()));
        assert!(ids_before.contains(&cfd_1.id()));

        assert!(!ids_after.contains(&trivial_cfd.id()));
        assert!(ids_after.contains(&cfd_0.id()));
        assert!(ids_after.contains(&cfd_1.id()));
    }
}
