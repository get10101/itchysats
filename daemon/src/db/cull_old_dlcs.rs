use crate::db::Connection;
use anyhow::Result;
use sqlx::pool::PoolConnection;
use sqlx::Sqlite;

impl Connection {
    /// Cull DLC data from old events.
    ///
    /// This method modifies the `dlc` field of the JSON blobs stored in the `events`
    /// table for all the rows which do not correspond to the most recent
    /// `EventKind::ContractSetupCompleted` or `EventKind::RolloverCompleted` event
    /// of a CFD.
    pub async fn cull_old_dlcs(&self) -> Result<()> {
        let mut conn = self.inner.acquire().await?;

        Connection::cull_rollover_completed(&mut conn).await?;
        Connection::cull_contract_setup_completed(&mut conn).await?;

        Ok(())
    }

    /// Cull old cfd rollover events which are not needed anymore
    ///
    /// We set the `dlc` field of events `RolloverCompleted` to null where the event is not the most
    /// recent `RolloverCompleted` event.
    async fn cull_rollover_completed(conn: &mut PoolConnection<Sqlite>) -> Result<()> {
        let affected_rows = sqlx::query!(
            r#"
            update EVENTS
                set
                    data = (
                        select json_set(events.data, '$.dlc', null)
                        from events as inner_events
                        where inner_events.id = EVENTS.id 
                    )
                where name = $1
                  and json_extract(EVENTS.data, '$.dlc.cets') is not null
                  and id not in (
                    select max(id) from EVENTS as inner_events
                    where inner_events.cfd_id = EVENTS.cfd_id
                      and inner_events.name = EVENTS.name
                );
            "#,
            model::EventKind::ROLLOVER_COMPLETED_EVENT,
        )
        .execute(&mut *conn)
        .await?
        .rows_affected();

        tracing::debug!(%affected_rows,"Culled RolloverCompleted events");

        Ok(())
    }

    /// Cull `ContractSetupCompleted` events which are not needed anymore
    ///
    /// `ContractSetupCompleted` events are not needed anymore if there was at least one
    /// `RolloverCompletedEvent` afterwards
    async fn cull_contract_setup_completed(conn: &mut PoolConnection<Sqlite>) -> Result<()> {
        let affected_rows = sqlx::query!(
            r#"
            update EVENTS
                set
                    data = (
                        select json_set(events.data, '$.dlc', null)
                        from events as inner_events
                        where EVENTS.id = inner_events.id
                    )
                where name = $1
                  and json_extract(EVENTS.data, '$.dlc.cets') is not null
                  and cfd_id in (
                    select distinct cfd_id from EVENTS as inner_events
                        where inner_events.cfd_id = EVENTS.cfd_id
                        and inner_events.name = $2
                    );
            "#,
            model::EventKind::CONTRACT_SETUP_COMPLETED_EVENT,
            model::EventKind::ROLLOVER_COMPLETED_EVENT,
        )
        .execute(&mut *conn)
        .await?
        .rows_affected();

        tracing::debug!(%affected_rows,"Culled ContractSetupCompleted events");

        Ok(())
    }
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
    async fn given_only_contract_setup_completed_and_no_rollover_when_culled_event_has_some_dlc() {
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

    #[tokio::test]
    async fn given_one_rollover_when_cull_then_first_event_has_none_dlc_and_second_has_some_dlc() {
        let db = memory().await.unwrap();

        let (cfd, contract_setup, rollovers) = rolled_over_cfd(NonZeroU8::new(1).unwrap());
        db.insert_cfd(&cfd).await.unwrap();
        db.append_event(contract_setup).await.unwrap();
        let only_rollover_event = rollovers.get(0).unwrap().clone().event;
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

        assert_eq!(events[1].event, only_rollover_event);
    }

    #[tokio::test]
    async fn given_two_rollovers_when_cull_old_dlcs_then_only_last_one_has_some_dlc() {
        let db = memory().await.unwrap();

        let (cfd, contract_setup, rollovers) = rolled_over_cfd(NonZeroU8::new(2).unwrap());
        db.insert_cfd(&cfd).await.unwrap();
        db.append_event(contract_setup).await.unwrap();
        let last_rollover_event = rollovers.last().unwrap().clone().event;
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

        assert_eq!(events[2].event, last_rollover_event);
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
    async fn given_cfds_contract_setup_completed_and_cfd_rolled_over_then_both_culled_correctly() {
        let db = memory().await.unwrap();

        let (cfd1, contract_setup) = contract_setup_completed_cfd();
        let contract_setup1_cloned = contract_setup.event.clone();
        db.insert_cfd(&cfd1).await.unwrap();
        db.append_event(contract_setup).await.unwrap();

        let (cfd2, contract_setup, rollovers) = rolled_over_cfd(NonZeroU8::new(2).unwrap());
        db.insert_cfd(&cfd2).await.unwrap();
        db.append_event(contract_setup).await.unwrap();
        let most_recent_rollover = rollovers.last().unwrap().event.clone();
        for rollover in rollovers {
            db.append_event(rollover).await.unwrap();
        }

        db.cull_old_dlcs().await.unwrap();

        let mut conn = db.inner.acquire().await.unwrap();
        let mut db_tx = conn.begin().await.unwrap();

        let events_cfd1 = load_cfd_events(&mut db_tx, cfd1.id(), 0).await.unwrap();
        let events_cfd2 = load_cfd_events(&mut db_tx, cfd2.id(), 0).await.unwrap();
        db_tx.commit().await.unwrap();

        assert_eq!(events_cfd1[0].event, contract_setup1_cloned,);

        assert!(matches!(
            &events_cfd2[0].event,
            EventKind::ContractSetupCompleted { dlc: None, .. },
        ));

        assert!(matches!(
            &events_cfd2[1].event,
            EventKind::RolloverCompleted { dlc: None, .. },
        ));

        assert_eq!(events_cfd2[2].event, most_recent_rollover);
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
