use crate::db::Connection;
use anyhow::Result;
use model::EventKind;
use sqlx::pool::PoolConnection;
use sqlx::Sqlite;

impl Connection {
    pub(crate) async fn cull_old_dlcs(&self) -> Result<()> {
        let mut conn = self.inner.acquire().await?;

        let emptied_dlc_events = emptied_dlc_events(&mut conn).await?;

        for (id, event) in emptied_dlc_events {
            if let Err(e) = update_event_data(&mut conn, id, event).await {
                tracing::warn!("Failed to overwrite old DLC: {e:#}");
            }
        }

        Ok(())
    }
}

/// Load all old events with DLC data from the database and return
/// them with empty `dlc` fields.
///
/// `EventKind::ContractSetupCompleted` an
/// `EventKind::RolloverCompleted` are the two kinds of event that can
/// have DLC data associated with them. One of these event is
/// considered old if it isn't the most recent one related to a CFD.
///
/// This method does not modify the database: it just loads rows and
/// processes them. The returned values can later be used to update
/// the contents of the database.
async fn emptied_dlc_events(
    conn: &mut PoolConnection<Sqlite>,
) -> Result<impl Iterator<Item = (i64, EventKind)>> {
    let rows = sqlx::query!(
        r#"
        SELECT
            id,
            name,
            data
        FROM
            events
        WHERE
            name = $1 OR
            name = $2
        ORDER BY
            created_at DESC
        LIMIT -1 OFFSET 1
        "#,
        model::EventKind::CONTRACT_SETUP_COMPLETED_EVENT,
        model::EventKind::ROLLOVER_COMPLETED_EVENT,
    )
    .fetch_all(&mut *conn)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let id = row.id;
            let event = match EventKind::from_json(row.name, row.data) {
                Ok(event) => Some(event),
                Err(e) => {
                    tracing::warn!("Failed to deserialize EventKind from JSON: {e:#}");
                    None
                }
            };

            event.map(|event| (id, event))
        })
        .filter_map(|(id, event)| {
            use EventKind::*;
            let event = match event {
                ContractSetupCompleted { dlc: Some(_), .. } => {
                    Some(ContractSetupCompleted { dlc: None })
                }
                RolloverCompleted {
                    dlc: Some(_),
                    funding_fee,
                } => Some(RolloverCompleted {
                    dlc: None,
                    funding_fee,
                }),
                // ignore events that have already been culled
                ContractSetupCompleted { dlc: None } | RolloverCompleted { dlc: None, .. } => None,
                // ignore all other events
                _ => None,
            };

            event.map(|event| (id, event))
        }))
}

/// Overwrite the contents of the `data` column for a row
/// corresponding to `id` with the JSON blob originated from
/// `updated_event`.
async fn update_event_data(
    conn: &mut PoolConnection<Sqlite>,
    id: i64,
    updated_event: EventKind,
) -> Result<()> {
    let (_, updated_data) = updated_event.to_json();

    let query_result = sqlx::query!(
        r#"
        UPDATE
            events
        SET
            data = $1
        WHERE
            id = $2
        "#,
        updated_data,
        id,
    )
    .execute(&mut *conn)
    .await?;

    if query_result.rows_affected() != 1 {
        anyhow::bail!("Failed to update event data");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::db::load_cfd_events;
    use crate::db::memory;
    use bdk::bitcoin::Amount;
    use model::libp2p::PeerId;
    use model::Cfd;
    use model::CfdEvent;
    use model::EventKind;
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
    use sqlx::Connection;
    use std::num::NonZeroU8;
    use time::Duration;

    #[tokio::test]
    async fn given_two_rollovers_when_cull_old_dlcs_then_only_last_one_has_some_dlc() {
        let db = memory().await.unwrap();

        let (cfd, contract_setup, rollovers) = rolled_over_cfd(NonZeroU8::new(2).unwrap());
        let most_recent_rollover = rollovers.last().unwrap().event.clone();

        db.insert_cfd(&cfd).await.unwrap();

        db.append_event(contract_setup).await.unwrap();

        for rollover in rollovers {
            db.append_event(rollover).await.unwrap();
        }

        db.cull_old_dlcs().await.unwrap();

        let mut conn = db.inner.acquire().await.unwrap();
        let mut db_tx = conn.begin().await.unwrap();
        let events = load_cfd_events(&mut db_tx, cfd.id(), 0).await.unwrap();
        db_tx.commit().await.unwrap();

        assert!(matches!(
            &events[0].event,
            EventKind::ContractSetupCompleted { dlc: None, .. },
        ));

        assert!(matches!(
            &events[1].event,
            EventKind::RolloverCompleted { dlc: None, .. },
        ));

        assert_eq!(events[2].event, most_recent_rollover);
    }

    #[tokio::test]
    async fn given_three_rollovers_when_cull_old_dlcs_then_only_last_one_has_some_dlc() {
        let db = memory().await.unwrap();

        let (cfd, contract_setup, rollovers) = rolled_over_cfd(NonZeroU8::new(3).unwrap());
        let most_recent_rollover = rollovers.last().unwrap().event.clone();

        db.insert_cfd(&cfd).await.unwrap();

        db.append_event(contract_setup).await.unwrap();

        for rollover in rollovers {
            db.append_event(rollover).await.unwrap();
        }

        db.cull_old_dlcs().await.unwrap();

        let mut conn = db.inner.acquire().await.unwrap();
        let mut db_tx = conn.begin().await.unwrap();
        let events = load_cfd_events(&mut db_tx, cfd.id(), 0).await.unwrap();
        db_tx.commit().await.unwrap();

        assert!(matches!(
            &events[0].event,
            EventKind::ContractSetupCompleted { dlc: None, .. },
        ));

        assert!(matches!(
            &events[1].event,
            EventKind::RolloverCompleted { dlc: None, .. },
        ));

        assert!(matches!(
            &events[2].event,
            EventKind::RolloverCompleted { dlc: None, .. },
        ));

        assert_eq!(events[3].event, most_recent_rollover);
    }

    #[tokio::test]
    async fn given_contract_setup_completed_when_cull_old_dlcs_then_dlc_is_some() {
        let db = memory().await.unwrap();

        let (cfd, contract_setup) = contract_setup_completed_cfd();
        let contract_setup_cloned = contract_setup.event.clone();

        db.insert_cfd(&cfd).await.unwrap();

        db.append_event(contract_setup).await.unwrap();

        db.cull_old_dlcs().await.unwrap();

        let mut conn = db.inner.acquire().await.unwrap();
        let mut db_tx = conn.begin().await.unwrap();
        let events = load_cfd_events(&mut db_tx, cfd.id(), 0).await.unwrap();
        db_tx.commit().await.unwrap();

        assert_eq!(events[0].event, contract_setup_cloned,);
    }

    fn contract_setup_completed_cfd() -> (Cfd, CfdEvent) {
        let order_id = OrderId::default();

        let cfd = Cfd::new(
            order_id,
            Position::Long,
            Price::new(dec!(41_772.8325)).unwrap(),
            Leverage::TWO,
            Duration::hours(24),
            Role::Taker,
            Usd::new(dec!(100)),
            "69a42aa90da8b065b9532b62bff940a3ba07dbbb11d4482c7db83a7e049a9f1e"
                .parse()
                .unwrap(),
            Some(PeerId::random()),
            OpeningFee::new(Amount::ZERO),
            FundingRate::default(),
            TxFeeRate::default(),
        );

        let contract_setup_completed =
            std::fs::read_to_string("./src/test_events/contract_setup_completed.json").unwrap();
        let contract_setup_completed =
            serde_json::from_str::<EventKind>(&contract_setup_completed).unwrap();
        let contract_setup_completed = CfdEvent {
            timestamp: Timestamp::new(0),
            id: order_id,
            event: contract_setup_completed,
        };

        (cfd, contract_setup_completed)
    }

    fn rolled_over_cfd(n: NonZeroU8) -> (Cfd, CfdEvent, Vec<CfdEvent>) {
        let (cfd, contract_setup_completed) = contract_setup_completed_cfd();
        let contract_setup_time = contract_setup_completed.timestamp.seconds();

        let n = u8::from(n) as i64;
        let rollovers = (1..=n)
            .map(|i| {
                let event =
                    std::fs::read_to_string("./src/test_events/rollover_completed.json").unwrap();
                let event = serde_json::from_str::<EventKind>(&event).unwrap();
                CfdEvent {
                    timestamp: Timestamp::new(contract_setup_time + i), /* to ensure consistent
                                                                         * order */
                    id: cfd.id(),
                    event,
                }
            })
            .collect();

        (cfd, contract_setup_completed, rollovers)
    }
}
