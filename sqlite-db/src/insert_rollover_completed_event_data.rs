use anyhow::Context;
use anyhow::Result;
use bdk::bitcoin::hashes::hex::ToHex;
use model::olivia::BitMexPriceEventId;
use model::Cet;
use model::CfdEvent;
use model::Dlc;
use model::EventKind;
use model::FundingFee;
use model::OrderId;
use model::RevokedCommit;
use sqlx::pool::PoolConnection;
use sqlx::Connection as SqlxConnection;
use sqlx::Sqlite;
use sqlx::Transaction;

pub async fn insert_rollover_completed_event(
    connection: &mut PoolConnection<Sqlite>,
    event_id: i64,
    event: CfdEvent,
) -> Result<()> {
    let event_kind = event.event;
    match event_kind {
        EventKind::RolloverCompleted {
            dlc: Some(dlc),
            funding_fee,
        } => {
            let mut inner_transaction = connection.begin().await?;

            delete_rollover_completed_event_data(&mut inner_transaction, event.id).await?;

            insert_rollover_completed_event_data(
                &mut inner_transaction,
                event_id,
                &dlc,
                funding_fee,
                event.id,
            )
            .await?;

            for revoked in dlc.revoked_commit {
                insert_revoked_commit_transaction(&mut inner_transaction, event.id, revoked)
                    .await?;
            }

            for (event_id, cets) in dlc.cets {
                for cet in cets {
                    insert_cet(&mut inner_transaction, event_id, event.id, cet).await?;
                }
            }

            // Commit the transaction to either write all or rollback
            inner_transaction.commit().await?;
        }
        EventKind::RolloverCompleted { dlc: None, .. } => {
            // We ignore rollover completed events without DLC data as we don't need to store
            // anything
        }
        _ => {
            tracing::error!("Invalid event type. Use `append_event` function instead")
        }
    }

    Ok(())
}

async fn delete_rollover_completed_event_data(
    inner_transaction: &mut Transaction<'_, Sqlite>,
    offer_id: OrderId,
) -> Result<()> {
    sqlx::query!(
        r#"
            delete from rollover_completed_event_data where cfd_id = (select id from cfds where cfds.uuid = $1)
        "#,
        offer_id
    )
        .execute(&mut *inner_transaction)
        .await
        .with_context(|| format!("Failed to delete from rollover_completed_event_data for {offer_id}"))?;

    sqlx::query!(
        r#"
            delete from revoked_commit_transactions where cfd_id = (select id from cfds where cfds.uuid = $1)
        "#,
        offer_id
    )
        .execute(&mut *inner_transaction)
        .await
        .with_context(|| format!("Failed to delete from revoked_commit_transactions for {offer_id}"))?;

    sqlx::query!(
        r#"
            delete from open_cets where cfd_id = (select id from cfds where cfds.uuid = $1)
        "#,
        offer_id
    )
    .execute(&mut *inner_transaction)
    .await
    .with_context(|| format!("Failed to delete from open_cets for {offer_id}"))?;

    Ok(())
}

/// Inserts RolloverCompleted data and returns the resulting rowid
async fn insert_rollover_completed_event_data(
    inner_transaction: &mut Transaction<'_, Sqlite>,
    event_id: i64,
    dlc: &Dlc,
    funding_fee: FundingFee,
    offer_id: OrderId,
) -> Result<()> {
    let (lock_tx, lock_tx_descriptor) = dlc.lock.clone();
    let (commit_tx, commit_adaptor_signature, commit_descriptor) = dlc.commit.clone();
    let (refund_tx, refund_signature) = dlc.refund.clone();

    // casting because u64 is not implemented for sqlx: https://github.com/launchbadge/sqlx/pull/919#discussion_r557256333
    let funding_fee_as_sat = funding_fee.fee.as_sat() as i64;
    // TODO: these seem to be redundant and should be in `cfds` table only
    let maker_lock_amount = dlc.maker_lock_amount.as_sat() as i64;
    let taker_lock_amount = dlc.taker_lock_amount.as_sat() as i64;

    let maker_address = dlc.maker_address.to_string();
    let taker_address = dlc.taker_address.to_string();

    let lock_tx_descriptor = lock_tx_descriptor.to_string();
    let commit_tx_descriptor = commit_descriptor.to_string();
    let refund_signature = refund_signature.to_string();
    let query_result = sqlx::query!(
        r#"
            insert into rollover_completed_event_data (
                cfd_id,
                event_id,
                settlement_event_id,
                refund_timelock,
                funding_fee,
                rate,
                identity,
                identity_counterparty,
                maker_address,
                taker_address,
                maker_lock_amount,
                taker_lock_amount,
                publish_sk,
                publish_pk_counterparty,
                revocation_secret,
                revocation_pk_counterparty,
                lock_tx,
                lock_tx_descriptor,
                commit_tx,
                commit_adaptor_signature,
                commit_descriptor,
                refund_tx,
                refund_signature
            ) values ( 
            (select id from cfds where cfds.uuid = $1),
            $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23
            )
        "#,
        offer_id,
        event_id,
        dlc.settlement_event_id,
        dlc.refund_timelock,
        funding_fee_as_sat,
        funding_fee.rate,
        dlc.identity,
        dlc.identity_counterparty,
        maker_address,
        taker_address,
        maker_lock_amount,
        taker_lock_amount,
        dlc.publish,
        dlc.publish_pk_counterparty,
        dlc.revocation,
        dlc.revocation_pk_counterparty,
        lock_tx,
        lock_tx_descriptor,
        commit_tx,
        commit_adaptor_signature,
        commit_tx_descriptor,
        refund_tx,
        refund_signature,
    )
    .execute(&mut *inner_transaction)
    .await?;

    if query_result.rows_affected() != 1 {
        anyhow::bail!("failed to insert rollover event data");
    }
    Ok(())
}

async fn insert_revoked_commit_transaction(
    inner_transaction: &mut Transaction<'_, Sqlite>,
    offer_id: OrderId,
    revoked: RevokedCommit,
) -> Result<()> {
    let revoked_tx_script_pubkey = revoked.script_pubkey.to_hex();
    let query_result = sqlx::query!(
        r#"
                insert into revoked_commit_transactions (
                    cfd_id,
                    encsig_ours,
                    publication_pk_theirs,
                    revocation_sk_theirs,
                    script_pubkey,
                    txid
                ) values ( (select id from cfds where cfds.uuid = $1), $2, $3, $4, $5, $6 )
            "#,
        offer_id,
        revoked.encsig_ours,
        revoked.publication_pk_theirs,
        revoked.revocation_sk_theirs,
        revoked_tx_script_pubkey,
        revoked.txid
    )
    .execute(&mut *inner_transaction)
    .await?;

    if query_result.rows_affected() != 1 {
        anyhow::bail!("failed to insert revoked transaction data");
    }
    Ok(())
}

async fn insert_cet(
    db_transaction: &mut Transaction<'_, Sqlite>,
    event_id: BitMexPriceEventId,
    offer_id: OrderId,
    cet: Cet,
) -> Result<()> {
    let maker_amount = cet.maker_amount.as_sat() as i64;
    let taker_amount = cet.taker_amount.as_sat() as i64;
    let n_bits = cet.n_bits as i64;
    let range_start = *cet.range.start() as i64;
    let range_end = *cet.range.end() as i64;

    let txid = cet.txid.to_string();
    let query_result = sqlx::query!(
        r#"
                insert into open_cets (
                    cfd_id,
                    oracle_event_id,
                    adaptor_sig,
                    maker_amount,
                    taker_amount,
                    n_bits,
                    range_start,
                    range_end,
                    txid
                ) values ( (select id from cfds where cfds.uuid = $1), $2, $3, $4, $5, $6, $7, $8, $9 )
            "#,
        offer_id,
        event_id,
        cet.adaptor_sig,
        maker_amount,
        taker_amount,
        n_bits,
        range_start,
        range_end,
        txid,
    )
    .execute(&mut *db_transaction)
    .await?;

    if query_result.rows_affected() != 1 {
        anyhow::bail!("failed to insert cet data");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory;
    use anyhow::bail;
    use bdk::bitcoin::Amount;
    use model::olivia::BitMexPriceEventId;
    use model::Cfd;
    use model::CfdEvent;
    use model::FundingRate;
    use model::Leverage;
    use model::OpeningFee;
    use model::OrderId;
    use model::Position;
    use model::Price;
    use model::Role;
    use model::Timestamp;
    use model::TxFeeRate;
    use model::Usd;
    use rust_decimal_macros::dec;
    use sqlx::pool::PoolConnection;
    use time::macros::datetime;
    use time::Duration;
    use time::OffsetDateTime;

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
            None,
            OpeningFee::new(Amount::from_sat(2000)),
            FundingRate::default(),
            TxFeeRate::default(),
        )
    }

    #[tokio::test]
    async fn test_insert_rollover_completed_event_data_should_not_error() {
        let db = memory().await.unwrap();
        let cfd = dummy_cfd();
        db.insert_cfd(&cfd).await.unwrap();
        let timestamp = Timestamp::now();
        let event = std::fs::read_to_string("./src/test_events/rollover_completed.json").unwrap();
        let event = serde_json::from_str::<EventKind>(&event).unwrap();
        let rollover_completed = CfdEvent {
            timestamp,
            id: cfd.id(),
            event,
        };

        let mut connection = db.inner.acquire().await.unwrap();
        db.append_event(rollover_completed.clone()).await.unwrap();
        insert_rollover_completed_event(&mut connection, 1, rollover_completed.clone())
            .await
            .unwrap();

        let (rollovers, revokes, cets) = count_table_entries(connection).await;
        assert_eq!(rollovers, 1);
        assert_eq!(revokes, 2);
        assert_eq!(cets, 2);
    }

    #[tokio::test]
    async fn repeatedly_insert_rollover_completed_event_data_should_not_error() -> Result<()> {
        let db = memory().await?;

        let cfd = dummy_cfd();
        db.insert_cfd(&cfd).await?;

        let timestamp = Timestamp::now();

        let event = std::fs::read_to_string("./src/test_events/rollover_completed.json")?;
        let event = serde_json::from_str::<EventKind>(&event)?;

        let rollover_completed = CfdEvent {
            timestamp,
            id: cfd.id(),
            event: event.clone(),
        };

        // insert first rollovercompleted event
        let mut connection = db.inner.acquire().await?;
        db.append_event(rollover_completed.clone()).await?;
        insert_rollover_completed_event(&mut connection, 1, rollover_completed.clone()).await?;

        // insert second rollovercompleted event with different event id
        let rollover_completed = update_event_id(
            timestamp,
            event,
            datetime!(2021-06-01 10:00:00).assume_utc(),
            cfd.id(),
        )?;
        let mut connection = db.inner.acquire().await?;
        db.append_event(rollover_completed.clone()).await?;
        insert_rollover_completed_event(&mut connection, 2, rollover_completed).await?;

        let (rollovers, revokes, cets) = count_table_entries(connection).await;
        assert_eq!(rollovers, 1);
        assert_eq!(revokes, 2);
        assert_eq!(cets, 2);

        Ok(())
    }

    async fn count_table_entries(mut connection: PoolConnection<Sqlite>) -> (i32, i32, i32) {
        let row = sqlx::query!(
            r#"
            SELECT 
                COUNT(DISTINCT rollover_completed_event_data.id) as rollovers, 
                COUNT(DISTINCT revoked_commit_transactions.id) as revokes, 
                COUNT(DISTINCT open_cets.id) as cets
            FROM 
                rollover_completed_event_data, 
                revoked_commit_transactions, 
                open_cets;
            "#
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        (
            row.rollovers,
            // Not sure why these are options
            row.revokes.unwrap(),
            row.cets.unwrap(),
        )
    }

    fn update_event_id(
        timestamp: Timestamp,
        event: EventKind,
        settlement_event_timestamp: OffsetDateTime,
        id: OrderId,
    ) -> Result<CfdEvent> {
        match event {
            EventKind::RolloverCompleted {
                dlc: Some(mut dlc),
                funding_fee,
            } => {
                dlc.settlement_event_id =
                    BitMexPriceEventId::with_20_digits(settlement_event_timestamp);

                Ok(CfdEvent {
                    timestamp,
                    id,
                    event: EventKind::RolloverCompleted {
                        dlc: Some(dlc),
                        funding_fee,
                    },
                })
            }
            _ => {
                bail!("We should always have a RolloverCompleted event")
            }
        }
    }
}
