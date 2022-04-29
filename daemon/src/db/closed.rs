//! This module allows us to move closed CFDs to separate tables so
//! that we can reason about loading them independently from open
//! CFDs.
//!
//! Therefore, it also provides an interface to load closed CFDs: the
//! `ClosedCfdAggregate` trait. Implementers of the trait will be able
//! to call the `crate::db::load_all_cfds` API, which loads all types
//! of CFD.

use crate::db::delete_from_cfds_table;
use crate::db::delete_from_events_table;
use crate::db::event_log::EventLog;
use crate::db::event_log::EventLogEntry;
use crate::db::load_cfd_events;
use crate::db::load_cfd_row;
use crate::db::Cfd;
use crate::db::CfdAggregate;
use crate::db::Connection;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use bdk::bitcoin::Amount;
use bdk::bitcoin::OutPoint;
use bdk::bitcoin::Script;
use bdk::miniscript::DescriptorTrait;
use maia::TransactionExt;
use model::long_and_short_leverage;
use model::CfdEvent;
use model::Contracts;
use model::Dlc;
use model::FeeAccount;
use model::Fees;
use model::FundingFee;
use model::Identity;
use model::Leverage;
use model::OrderId;
use model::Payout;
use model::Position;
use model::Price;
use model::Role;
use model::Timestamp;
use model::Txid;
use model::Vout;
use model::SETTLEMENT_INTERVAL;
use sqlx::pool::PoolConnection;
use sqlx::Connection as _;
use sqlx::Sqlite;
use sqlx::Transaction;
use time::OffsetDateTime;

/// A trait for building an aggregate based on a `ClosedCfd`.
pub trait ClosedCfdAggregate: CfdAggregate {
    fn new_closed(args: Self::CtorArgs, cfd: ClosedCfd) -> Self;
}

/// Data loaded from the database about a closed CFD.
#[derive(Debug, Clone, Copy)]
pub struct ClosedCfd {
    pub id: OrderId,
    pub position: Position,
    pub initial_price: Price,
    pub taker_leverage: Leverage,
    pub n_contracts: Contracts,
    pub counterparty_network_identity: Identity,
    pub role: Role,
    pub fees: Fees,
    pub expiry_timestamp: OffsetDateTime,
    pub lock: Lock,
    pub settlement: Settlement,
    pub creation_timestamp: Timestamp,
}

/// Data loaded from the database about the lock transaction of a
/// closed CFD.
#[derive(Debug, Clone, Copy)]
pub struct Lock {
    pub txid: Txid,
    pub dlc_vout: Vout,
}

/// Representation of how a closed CFD was settled.
///
/// It is represented using an `enum` rather than a series of optional
/// fields so that only sane combinations of transactions can be
/// loaded from the database.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Settlement {
    Collaborative {
        txid: Txid,
        vout: Vout,
        payout: Payout,
        price: Price,
    },
    Cet {
        commit_txid: Txid,
        txid: Txid,
        vout: Vout,
        payout: Payout,
        price: Price,
    },
    Refund {
        commit_txid: Txid,
        txid: Txid,
        vout: Vout,
        payout: Payout,
    },
}

impl Connection {
    pub async fn move_to_closed_cfds(&self) -> Result<()> {
        let ids = self.closed_cfd_ids_according_to_the_blockchain().await?;

        if !ids.is_empty() {
            tracing::debug!("Moving CFDs to closed_cfds table: {ids:?}");
        }

        for id in ids.into_iter() {
            let pool = self.inner.clone();
            let fut = async move {
                let mut conn = pool.acquire().await?;
                let mut db_tx = conn.begin().await?;

                let cfd = load_cfd_row(&mut db_tx, id).await?;
                let events = load_cfd_events(&mut db_tx, id, 0).await?;
                let event_log = EventLog::new(&events);

                let closed_cfd = ClosedCfdInputAggregate::new(cfd);
                let closed_cfd = events
                    .into_iter()
                    .try_fold(closed_cfd, ClosedCfdInputAggregate::apply)?
                    .build()?;

                insert_closed_cfd(&mut db_tx, closed_cfd).await?;
                insert_event_log(&mut db_tx, id, event_log).await?;

                insert_settlement(&mut db_tx, id, closed_cfd.settlement).await?;

                delete_from_events_table(&mut db_tx, id).await?;
                delete_from_cfds_table(&mut db_tx, id).await?;

                db_tx.commit().await?;

                anyhow::Ok(())
            };

            match fut.await {
                Ok(()) => tracing::debug!(order_id = %id, "Moved CFD to `closed_cfds` table"),
                Err(e) => tracing::warn!(order_id = %id, "Failed to move closed CFD: {e:#}"),
            }
        }

        Ok(())
    }

    /// Load a closed CFD from the database.
    pub(super) async fn load_closed_cfd<C>(&self, id: OrderId, args: C::CtorArgs) -> Result<C>
    where
        C: ClosedCfdAggregate,
    {
        let mut conn = self.inner.acquire().await?;

        let cfd = sqlx::query!(
            r#"
            SELECT
                uuid as "uuid: model::OrderId",
                position as "position: model::Position",
                initial_price as "initial_price: model::Price",
                taker_leverage as "taker_leverage: model::Leverage",
                n_contracts as "n_contracts: model::Contracts",
                counterparty_network_identity as "counterparty_network_identity: model::Identity",
                role as "role: model::Role",
                fees as "fees: model::Fees",
                expiry_timestamp,
                lock_txid as "lock_txid: model::Txid",
                lock_dlc_vout as "lock_dlc_vout: model::Vout"
            FROM
                closed_cfds
            WHERE
                closed_cfds.uuid = $1
            "#,
            id
        )
        .fetch_one(&mut conn)
        .await?;

        let expiry_timestamp = OffsetDateTime::from_unix_timestamp(cfd.expiry_timestamp)?;

        let collaborative_settlement = load_collaborative_settlement(&mut conn, id).await?;
        let cet_settlement = load_cet_settlement(&mut conn, id).await?;
        let refund_settlement = load_refund_settlement(&mut conn, id).await?;

        let settlement = match (collaborative_settlement, cet_settlement, refund_settlement) {
            (Some(collaborative_settlement), None, None) => collaborative_settlement,
            (None, Some(cet), None) => cet,
            (None, None, Some(refund)) => refund,
            _ => {
                bail!(
                    "Closed CFD has insane combination of transactions:
                       {collaborative_settlement:?},
                       {cet_settlement:?},
                       {refund_settlement:?}"
                )
            }
        };

        let creation_timestamp = load_creation_timestamp(&mut conn, id).await?;

        let cfd = ClosedCfd {
            id,
            position: cfd.position,
            initial_price: cfd.initial_price,
            taker_leverage: cfd.taker_leverage,
            n_contracts: cfd.n_contracts,
            counterparty_network_identity: cfd.counterparty_network_identity,
            role: cfd.role,
            fees: cfd.fees,
            expiry_timestamp,
            lock: Lock {
                txid: cfd.lock_txid,
                dlc_vout: cfd.lock_dlc_vout,
            },
            settlement,
            creation_timestamp,
        };

        Ok(C::new_closed(args, cfd))
    }

    pub(super) async fn load_closed_cfd_ids(&self) -> Result<Vec<OrderId>> {
        let mut conn = self.inner.acquire().await?;

        let ids = sqlx::query!(
            r#"
            SELECT
                uuid as "uuid: model::OrderId"
            FROM
                closed_cfds
            "#
        )
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .map(|r| r.uuid)
        .collect();

        Ok(ids)
    }
}

/// Auxiliary type used to gradually combine a `Cfd` with its list of
/// `CfdEvent`s.
///
/// Once all the `CfdEvent`s have been applied, we can build a
/// `ClosedCfdInput` which is used for database insertion.
#[derive(Debug, Clone)]
struct ClosedCfdInputAggregate {
    id: OrderId,
    position: Position,
    initial_price: Price,
    taker_leverage: Leverage,
    n_contracts: Contracts,
    counterparty_network_identity: Identity,
    role: Role,
    fee_account: FeeAccount,
    initial_funding_fee: FundingFee,
    latest_dlc: Option<Dlc>,
    collaborative_settlement: Option<(bdk::bitcoin::Transaction, Script, Price)>,
    cet: Option<(bdk::bitcoin::Transaction, Price)>,
    cet_confirmed: bool,
    collaborative_settlement_confirmed: bool,
    refund_confirmed: bool,
}

impl ClosedCfdInputAggregate {
    fn new(cfd: Cfd) -> Self {
        let Cfd {
            id,
            position,
            initial_price,
            taker_leverage,
            settlement_interval: _,
            quantity_usd,
            counterparty_network_identity,
            role,
            opening_fee,
            initial_funding_rate,
            ..
        } = cfd;
        let n_contracts = quantity_usd
            .try_into_u64()
            .expect("number of contracts to fit into a u64");
        let n_contracts = Contracts::new(n_contracts);

        let initial_funding_fee = {
            let (long_leverage, short_leverage) =
                long_and_short_leverage(taker_leverage, role, position);

            FundingFee::calculate(
                initial_price,
                quantity_usd,
                long_leverage,
                short_leverage,
                initial_funding_rate,
                SETTLEMENT_INTERVAL.whole_hours(),
            )
            .expect("values from db to be sane")
        };

        Self {
            id,
            position,
            initial_price,
            taker_leverage,
            n_contracts,
            counterparty_network_identity,
            role,
            fee_account: FeeAccount::new(position, role).add_opening_fee(opening_fee),
            initial_funding_fee,
            latest_dlc: None,
            collaborative_settlement: None,
            cet: None,
            cet_confirmed: false,
            collaborative_settlement_confirmed: false,
            refund_confirmed: false,
        }
    }

    fn apply(mut self, event: CfdEvent) -> Result<Self> {
        use model::EventKind::*;
        match event.event {
            ContractSetupStarted => {}
            ContractSetupCompleted { dlc } => {
                self.fee_account = self.fee_account.add_funding_fee(self.initial_funding_fee);
                self.latest_dlc = Some(dlc);
            }
            ContractSetupFailed => {}
            OfferRejected => {}
            RolloverStarted => {}
            RolloverAccepted => {}
            RolloverRejected => {}
            RolloverCompleted { dlc, funding_fee } => {
                self.fee_account = self.fee_account.add_funding_fee(funding_fee);
                self.latest_dlc = Some(dlc);
            }
            RolloverFailed => {}
            CollaborativeSettlementStarted { .. } => {}
            CollaborativeSettlementProposalAccepted => {}
            CollaborativeSettlementCompleted {
                spend_tx,
                script,
                price,
            } => {
                self.collaborative_settlement = Some((spend_tx, script, price));
            }
            CollaborativeSettlementRejected => {}
            CollaborativeSettlementFailed => {}
            LockConfirmed => {}
            LockConfirmedAfterFinality => {}
            CommitConfirmed => {}
            CetConfirmed => {
                self.cet_confirmed = true;
            }
            RefundConfirmed => {
                self.refund_confirmed = true;
            }
            RevokeConfirmed => {}
            CollaborativeSettlementConfirmed => {
                self.collaborative_settlement_confirmed = true;
            }
            CetTimelockExpiredPriorOracleAttestation => {}
            CetTimelockExpiredPostOracleAttestation { cet: _ } => {
                // if we have an attestation we have already updated
                // the `self.cet` field in the
                // `OracleAttestedPriorCetTimelock` branch.
                //
                // We could repeat that work here just in case, but we
                // don't have the closing price, so the `Cet` struct
                // would be incomplete
            }
            RefundTimelockExpired { .. } => {}
            OracleAttestedPriorCetTimelock {
                timelocked_cet,
                price,
                ..
            } => {
                self.cet = Some((timelocked_cet, price));
            }
            OracleAttestedPostCetTimelock { cet, price } => {
                self.cet = Some((cet, price));
            }
            ManualCommit { .. } => {}
        }

        Ok(self)
    }

    fn latest_dlc(&self) -> Result<&Dlc> {
        match self.latest_dlc {
            None => {
                bail!("No DLC after commit confirmed");
            }
            Some(ref dlc) => Ok(dlc),
        }
    }

    fn lock(&self) -> Result<Lock> {
        let script_pubkey = self.latest_dlc()?.lock.1.script_pubkey();
        let OutPoint { txid, vout } = self
            .latest_dlc()?
            .lock
            .0
            .outpoint(&script_pubkey)
            .context("Missing DLC in lock TX")?;

        let txid = Txid::new(txid);
        let dlc_vout = Vout::new(vout);

        Ok(Lock { txid, dlc_vout })
    }

    fn collaborative_settlement(&self) -> Result<Settlement> {
        let (spend_tx, script, price) = self
            .collaborative_settlement
            .as_ref()
            .context("Collaborative settlement not set")?;

        let OutPoint { txid, vout } = spend_tx
            .outpoint(script)
            .context("Missing spend script in collaborative settlement TX")?;

        let payout = &spend_tx
            .output
            .get(vout as usize)
            .with_context(|| format!("No output at vout {vout}"))?;
        let payout = Payout::new(Amount::from_sat(payout.value));

        let txid = Txid::new(txid);
        let vout = Vout::new(vout);

        Ok(Settlement::Collaborative {
            txid,
            vout,
            payout,
            price: *price,
        })
    }

    fn cet(&self) -> Result<Settlement> {
        let (cet, price) = self.cet.as_ref().context("Cet not set")?;

        let commit_txid = Txid::new(self.latest_dlc()?.commit.0.txid());

        let own_script_pubkey = self.latest_dlc()?.script_pubkey_for(self.role);

        let OutPoint { txid, vout } = cet
            .outpoint(&own_script_pubkey)
            .context("Missing spend script in CET")?;

        let payout = &cet
            .output
            .get(vout as usize)
            .with_context(|| format!("No output at vout {vout}"))?;
        let payout = Payout::new(Amount::from_sat(payout.value));

        let txid = Txid::new(txid);
        let vout = Vout::new(vout);

        Ok(Settlement::Cet {
            commit_txid,
            txid,
            vout,
            payout,
            price: *price,
        })
    }

    fn refund(&self) -> Result<Settlement> {
        let dlc = self.latest_dlc()?;

        let own_script_pubkey = dlc.script_pubkey_for(self.role);
        let refund_tx = &dlc.refund.0;

        let OutPoint { txid, vout } = refund_tx
            .outpoint(&own_script_pubkey)
            .context("Missing spend script in refund TX")?;

        let payout = &refund_tx
            .output
            .get(vout as usize)
            .with_context(|| format!("No output at vout {vout}"))?;
        let payout = Payout::new(Amount::from_sat(payout.value));

        let commit_txid = Txid::new(dlc.commit.0.txid());
        let txid = Txid::new(txid);
        let vout = Vout::new(vout);

        Ok(Settlement::Refund {
            commit_txid,
            txid,
            vout,
            payout,
        })
    }

    fn build(self) -> Result<ClosedCfdInput> {
        let Self {
            id,
            position,
            initial_price,
            taker_leverage,
            n_contracts,
            counterparty_network_identity,
            role,
            fee_account,
            ..
        } = self;

        let lock = self.lock()?;
        let dlc = self.latest_dlc()?;

        let settlement = match (
            self.collaborative_settlement_confirmed,
            self.cet_confirmed,
            self.refund_confirmed,
        ) {
            (true, false, false) => self.collaborative_settlement()?,
            (false, true, false) => self.cet()?,
            (false, false, true) => self.refund()?,
            (collaborative_settlement, cet, refund) => bail!(
                "Insane transaction combination:
                    Collaborative settlement: {collaborative_settlement:?},
                    CET: {cet:?},
                    Refund: {refund:?},"
            ),
        };

        Ok(ClosedCfdInput {
            id,
            position,
            initial_price,
            taker_leverage,
            n_contracts,
            counterparty_network_identity,
            role,
            fees: Fees::new(fee_account.balance()),
            expiry_timestamp: dlc.settlement_event_id.timestamp(),
            lock,
            settlement,
        })
    }
}

/// All the data related to a closed CFD that we want to store in the
/// database.
#[derive(Debug, Clone, Copy)]
struct ClosedCfdInput {
    id: OrderId,
    position: Position,
    initial_price: Price,
    taker_leverage: Leverage,
    n_contracts: Contracts,
    counterparty_network_identity: Identity,
    role: Role,
    fees: Fees,
    expiry_timestamp: OffsetDateTime,
    lock: Lock,
    settlement: Settlement,
}

async fn insert_closed_cfd(conn: &mut Transaction<'_, Sqlite>, cfd: ClosedCfdInput) -> Result<()> {
    let expiry_timestamp = cfd.expiry_timestamp.unix_timestamp();

    let query_result = sqlx::query!(
        r#"
        INSERT INTO closed_cfds
        (
            uuid,
            position,
            initial_price,
            taker_leverage,
            n_contracts,
            counterparty_network_identity,
            role,
            fees,
            expiry_timestamp,
            lock_txid,
            lock_dlc_vout
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        "#,
        cfd.id,
        cfd.position,
        cfd.initial_price,
        cfd.taker_leverage,
        cfd.n_contracts,
        cfd.counterparty_network_identity,
        cfd.role,
        cfd.fees,
        expiry_timestamp,
        cfd.lock.txid,
        cfd.lock.dlc_vout,
    )
    .execute(&mut *conn)
    .await?;

    if query_result.rows_affected() != 1 {
        anyhow::bail!("failed to insert into closed_cfds");
    }

    Ok(())
}

async fn insert_settlement(
    conn: &mut Transaction<'_, Sqlite>,
    id: OrderId,
    settlement: Settlement,
) -> Result<()> {
    match settlement {
        Settlement::Collaborative {
            txid,
            vout,
            payout,
            price,
        } => insert_collaborative_settlement(conn, id, txid, vout, payout, price).await?,
        Settlement::Cet {
            commit_txid,
            txid,
            vout,
            payout,
            price,
        } => insert_cet_settlement(conn, id, commit_txid, txid, vout, payout, price).await?,
        Settlement::Refund {
            commit_txid,
            txid,
            vout,
            payout,
        } => insert_refund_settlement(conn, id, commit_txid, txid, vout, payout).await?,
    };

    Ok(())
}

async fn insert_collaborative_settlement(
    conn: &mut Transaction<'_, Sqlite>,
    id: OrderId,
    txid: Txid,
    vout: Vout,
    payout: Payout,
    price: Price,
) -> Result<()> {
    let query_result = sqlx::query!(
        r#"
        INSERT INTO collaborative_settlement_txs
        (
            cfd_id,
            txid,
            vout,
            payout,
            price
        )
        VALUES
        (
            (SELECT id FROM closed_cfds WHERE closed_cfds.uuid = $1),
            $2, $3, $4, $5
        )
        "#,
        id,
        txid,
        vout,
        payout,
        price,
    )
    .execute(&mut *conn)
    .await?;

    if query_result.rows_affected() != 1 {
        anyhow::bail!("failed to insert into collaborative_settlement_txs");
    }

    Ok(())
}

async fn insert_cet_settlement(
    conn: &mut Transaction<'_, Sqlite>,
    id: OrderId,
    commit_txid: Txid,
    txid: Txid,
    vout: Vout,
    payout: Payout,
    price: Price,
) -> Result<()> {
    insert_commit_tx(conn, id, commit_txid).await?;

    let query_result = sqlx::query!(
        r#"
        INSERT INTO cets
        (
            cfd_id,
            txid,
            vout,
            payout,
            price
        )
        VALUES
        (
            (SELECT id FROM closed_cfds WHERE closed_cfds.uuid = $1),
            $2, $3, $4, $5
        )
        "#,
        id,
        txid,
        vout,
        payout,
        price
    )
    .execute(&mut *conn)
    .await?;

    if query_result.rows_affected() != 1 {
        anyhow::bail!("failed to insert into cets");
    }

    Ok(())
}

async fn insert_refund_settlement(
    conn: &mut Transaction<'_, Sqlite>,
    id: OrderId,
    commit_txid: Txid,
    txid: Txid,
    vout: Vout,
    payout: Payout,
) -> Result<()> {
    insert_commit_tx(conn, id, commit_txid).await?;

    let query_result = sqlx::query!(
        r#"
        INSERT INTO refund_txs
        (
            cfd_id,
            txid,
            vout,
            payout
        )
        VALUES
        (
            (SELECT id FROM closed_cfds WHERE closed_cfds.uuid = $1),
            $2, $3, $4
        )
        "#,
        id,
        txid,
        vout,
        payout,
    )
    .execute(&mut *conn)
    .await?;

    if query_result.rows_affected() != 1 {
        anyhow::bail!("failed to insert into refund_txs");
    }

    Ok(())
}

async fn insert_commit_tx(
    conn: &mut Transaction<'_, Sqlite>,
    id: OrderId,
    txid: Txid,
) -> Result<()> {
    let query_result = sqlx::query!(
        r#"
        INSERT INTO commit_txs
        (
            cfd_id,
            txid
        )
        VALUES
        (
            (SELECT id FROM closed_cfds WHERE closed_cfds.uuid = $1),
            $2
        )
        "#,
        id,
        txid
    )
    .execute(&mut *conn)
    .await?;

    if query_result.rows_affected() != 1 {
        anyhow::bail!("failed to insert into commit_txs");
    }

    Ok(())
}

async fn load_collaborative_settlement(
    conn: &mut PoolConnection<Sqlite>,
    id: OrderId,
) -> Result<Option<Settlement>> {
    let row = sqlx::query_as!(
        Settlement::Collaborative,
        r#"
        SELECT
            collaborative_settlement_txs.txid as "txid: model::Txid",
            collaborative_settlement_txs.vout as "vout: model::Vout",
            collaborative_settlement_txs.payout as "payout: model::Payout",
            collaborative_settlement_txs.price as "price: model::Price"
        FROM
            collaborative_settlement_txs
        JOIN
            closed_cfds on closed_cfds.id = collaborative_settlement_txs.cfd_id
        WHERE
            closed_cfds.uuid = $1
        "#,
        id
    )
    .fetch_optional(&mut *conn)
    .await?;

    Ok(row)
}

async fn load_cet_settlement(
    conn: &mut PoolConnection<Sqlite>,
    id: OrderId,
) -> Result<Option<Settlement>> {
    let row = sqlx::query_as!(
        Settlement::Cet,
        r#"
        SELECT
            commit_txs.txid as "commit_txid: model::Txid",
            cets.txid as "txid: model::Txid",
            cets.vout as "vout: model::Vout",
            cets.payout as "payout: model::Payout",
            cets.price as "price: model::Price"
        FROM
            cets
        JOIN
            commit_txs on commit_txs.cfd_id = cets.cfd_id
        JOIN
            closed_cfds on closed_cfds.id = cets.cfd_id
        WHERE
            closed_cfds.uuid = $1
        "#,
        id
    )
    .fetch_optional(&mut *conn)
    .await?;

    Ok(row)
}

async fn load_refund_settlement(
    conn: &mut PoolConnection<Sqlite>,
    id: OrderId,
) -> Result<Option<Settlement>> {
    let row = sqlx::query_as!(
        Settlement::Refund,
        r#"
        SELECT
            commit_txs.txid as "commit_txid: model::Txid",
            refund_txs.txid as "txid: model::Txid",
            refund_txs.vout as "vout: model::Vout",
            refund_txs.payout as "payout: model::Payout"
        FROM
            refund_txs
        JOIN
            commit_txs on commit_txs.cfd_id = refund_txs.cfd_id
        JOIN
            closed_cfds on closed_cfds.id = refund_txs.cfd_id
        WHERE
            closed_cfds.uuid = $1
        "#,
        id
    )
    .fetch_optional(&mut *conn)
    .await?;

    Ok(row)
}

async fn insert_event_log(
    conn: &mut Transaction<'_, Sqlite>,
    id: OrderId,
    event_log: EventLog,
) -> Result<()> {
    for EventLogEntry { name, created_at } in event_log.0.iter() {
        let query_result = sqlx::query!(
            r#"
            INSERT INTO event_log (
                cfd_id,
                name,
                created_at
            )
            VALUES
            (
                (SELECT id FROM closed_cfds WHERE closed_cfds.uuid = $1),
                $2, $3
            )
            "#,
            id,
            name,
            created_at
        )
        .execute(&mut *conn)
        .await?;

        if query_result.rows_affected() != 1 {
            anyhow::bail!("failed to insert into event_log");
        }
    }

    Ok(())
}

/// Obtain the time at which the closed CFD was created, according to
/// the `event_log` table.
///
/// Every closed CFD must have gone through contract setup at some
/// point. Therefore, we base the creation timestamp on the
/// `EventKind::ContractSetupStarted` variant.
async fn load_creation_timestamp(
    conn: &mut PoolConnection<Sqlite>,
    id: OrderId,
) -> Result<Timestamp> {
    let row = sqlx::query!(
        r#"
        SELECT
            event_log.created_at
        FROM
            event_log
        JOIN
            closed_cfds on closed_cfds.id = event_log.cfd_id
        WHERE
            closed_cfds.uuid = $1 AND event_log.name = $2
        "#,
        id,
        model::EventKind::CONTRACT_SETUP_STARTED,
    )
    .fetch_one(&mut *conn)
    .await?;

    Ok(Timestamp::new(row.created_at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::memory;
    use bdk::bitcoin::SignedAmount;
    use model::Cfd;
    use model::EventKind;
    use model::FundingRate;
    use model::OpeningFee;
    use model::Timestamp;
    use model::TxFeeRate;
    use model::Usd;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::str::FromStr;
    use time::Duration;

    #[tokio::test]
    async fn given_confirmed_settlement_when_move_cfds_to_closed_table_then_can_load_cfd_as_closed()
    {
        let db = memory().await.unwrap();

        let (cfd, contract_setup_completed, collaborative_settlement_completed) =
            cfd_collaboratively_settled();
        let order_id = cfd.id();

        db.insert_cfd(&cfd).await.unwrap();

        db.append_event(contract_setup_completed).await.unwrap();
        db.append_event(collaborative_settlement_completed)
            .await
            .unwrap();
        db.append_event(collab_settlement_confirmed(&cfd))
            .await
            .unwrap();

        db.move_to_closed_cfds().await.unwrap();

        let load_from_open = db.load_open_cfd::<DummyAggregate>(order_id, ()).await;
        let load_from_events = {
            let mut conn = db.inner.acquire().await.unwrap();
            let mut db_tx = conn.begin().await.unwrap();
            let res = load_cfd_events(&mut db_tx, order_id, 0).await.unwrap();
            db_tx.commit().await.unwrap();

            res
        };
        let load_from_closed = db.load_closed_cfd::<DummyAggregate>(order_id, ()).await;

        assert!(load_from_open.is_err());
        assert!(load_from_events.is_empty());
        assert!(load_from_closed.is_ok());
    }

    #[tokio::test]
    async fn given_settlement_not_confirmed_when_move_cfds_to_closed_table_then_cannot_load_cfd_as_closed(
    ) {
        let db = memory().await.unwrap();

        let (cfd, contract_setup_completed, collaborative_settlement_completed) =
            cfd_collaboratively_settled();
        let order_id = cfd.id();

        db.insert_cfd(&cfd).await.unwrap();

        db.append_event(contract_setup_completed).await.unwrap();
        db.append_event(collaborative_settlement_completed)
            .await
            .unwrap();

        db.move_to_closed_cfds().await.unwrap();

        let load_from_open = db.load_open_cfd::<DummyAggregate>(order_id, ()).await;
        let load_from_events = {
            let mut conn = db.inner.acquire().await.unwrap();
            let mut db_tx = conn.begin().await.unwrap();
            let res = load_cfd_events(&mut db_tx, order_id, 0).await.unwrap();
            db_tx.commit().await.unwrap();

            res
        };
        let load_from_closed = db.load_closed_cfd::<DummyAggregate>(order_id, ()).await;

        assert!(load_from_open.is_ok());
        assert_eq!(load_from_events.len(), 2);
        assert!(load_from_closed.is_err());
    }

    #[tokio::test]
    async fn given_confirmed_settlement_when_move_cfds_to_closed_table_then_projection_aggregate_stays_the_same(
    ) {
        let db = memory().await.unwrap();

        let (cfd, contract_setup_completed, collaborative_settlement_completed) =
            cfd_collaboratively_settled();
        let order_id = cfd.id();

        db.insert_cfd(&cfd).await.unwrap();

        db.append_event(contract_setup_completed).await.unwrap();
        db.append_event(collaborative_settlement_completed)
            .await
            .unwrap();

        db.append_event(collab_settlement_confirmed(&cfd))
            .await
            .unwrap();

        let projection_open = {
            let projection_open = db
                .load_open_cfd::<crate::projection::Cfd>(order_id, bdk::bitcoin::Network::Testnet)
                .await
                .unwrap();
            projection_open.with_current_quote(None) // unconditional processing in `projection`
        };

        db.move_to_closed_cfds().await.unwrap();

        let projection_closed = {
            let projection_closed = db
                .load_closed_cfd::<crate::projection::Cfd>(order_id, bdk::bitcoin::Network::Testnet)
                .await
                .unwrap();
            projection_closed.with_current_quote(None) // unconditional processing in `projection`
        };

        // this comparison actually omits the `aggregated` field on
        // `projection::Cfd` because it is not used when aggregating
        // from a closed CFD
        assert_eq!(projection_open, projection_closed);
    }

    #[tokio::test]
    async fn insert_cet_roundtrip() {
        let db = memory().await.unwrap();

        let mut conn = db.inner.acquire().await.unwrap();
        let mut db_tx = conn.begin().await.unwrap();

        let id = OrderId::default();

        insert_dummy_closed_cfd(&mut db_tx, id).await.unwrap();

        let inserted = Settlement::Cet {
            commit_txid: Txid::new(bdk::bitcoin::Txid::default()),
            txid: Txid::new(bdk::bitcoin::Txid::default()),
            vout: Vout::new(0),
            payout: Payout::new(Amount::ONE_BTC),
            price: Price::new(dec!(40_000)).unwrap(),
        };

        insert_settlement(&mut db_tx, id, inserted).await.unwrap();
        db_tx.commit().await.unwrap();

        let loaded = load_cet_settlement(&mut conn, id).await.unwrap().unwrap();

        assert_eq!(inserted, loaded);
    }

    #[tokio::test]
    async fn given_inserting_different_settlements_then_we_can_load_them_again_correctly() {
        let db = memory().await.unwrap();

        let mut conn = db.inner.acquire().await.unwrap();

        let id_cet = OrderId::default();
        let id_refund = OrderId::default();
        let id_collab = OrderId::default();

        let commit_txid_cet = bdk::bitcoin::Txid::from_str(
            "684443dd37119031701f2a8caaaae8af5f1c7d7e7d55c3866d51b26609ae841f",
        )
        .unwrap();
        let commit_txid_refund = bdk::bitcoin::Txid::from_str(
            "9721ca44bdab8b0d9dd550b59efe6334554c7e8e242b019119b85107aac55a83",
        )
        .unwrap();

        let inserted_cet = Settlement::Cet {
            commit_txid: Txid::new(commit_txid_cet),
            txid: Txid::new(bdk::bitcoin::Txid::default()),
            vout: Vout::new(0),
            payout: Payout::new(Amount::ONE_BTC),
            price: Price::new(dec!(40_000)).unwrap(),
        };

        let inserted_collab_settlement = Settlement::Collaborative {
            txid: Txid::new(bdk::bitcoin::Txid::default()),
            vout: Vout::new(0),
            payout: Payout::new(Amount::ONE_BTC),
            price: Price::new(Decimal::ONE_HUNDRED).unwrap(),
        };

        let inserted_refund_settlement = Settlement::Refund {
            commit_txid: Txid::new(commit_txid_refund),
            txid: Txid::new(bdk::bitcoin::Txid::default()),
            vout: Vout::new(0),
            payout: Payout::new(Amount::ONE_BTC),
        };

        let mut db_tx = conn.begin().await.unwrap();
        insert_dummy_closed_cfd(&mut db_tx, id_cet).await.unwrap();
        insert_settlement(&mut db_tx, id_cet, inserted_cet)
            .await
            .unwrap();
        db_tx.commit().await.unwrap();

        let mut db_tx = conn.begin().await.unwrap();
        insert_dummy_closed_cfd(&mut db_tx, id_collab)
            .await
            .unwrap();
        insert_settlement(&mut db_tx, id_collab, inserted_collab_settlement)
            .await
            .unwrap();
        db_tx.commit().await.unwrap();

        let mut db_tx = conn.begin().await.unwrap();
        insert_dummy_closed_cfd(&mut db_tx, id_refund)
            .await
            .unwrap();
        insert_settlement(&mut db_tx, id_refund, inserted_refund_settlement)
            .await
            .unwrap();
        db_tx.commit().await.unwrap();

        let loaded_cet = load_cet_settlement(&mut conn, id_cet)
            .await
            .unwrap()
            .unwrap();
        let loaded_collab = load_collaborative_settlement(&mut conn, id_collab)
            .await
            .unwrap()
            .unwrap();
        let loaded_refund = load_refund_settlement(&mut conn, id_refund)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(inserted_cet, loaded_cet);
        assert_eq!(inserted_collab_settlement, loaded_collab);
        assert_eq!(inserted_refund_settlement, loaded_refund);
    }

    #[tokio::test]
    async fn insert_collaborative_settlement_tx_roundtrip() {
        let db = memory().await.unwrap();

        let mut conn = db.inner.acquire().await.unwrap();
        let mut db_tx = conn.begin().await.unwrap();

        let id = OrderId::default();

        insert_dummy_closed_cfd(&mut db_tx, id).await.unwrap();

        let inserted = Settlement::Collaborative {
            txid: Txid::new(bdk::bitcoin::Txid::default()),
            vout: Vout::new(0),
            payout: Payout::new(Amount::ONE_BTC),
            price: Price::new(dec!(40_000)).unwrap(),
        };

        insert_settlement(&mut db_tx, id, inserted).await.unwrap();
        db_tx.commit().await.unwrap();

        let loaded = load_collaborative_settlement(&mut conn, id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(inserted, loaded);
    }

    #[tokio::test]
    async fn insert_refund_tx_roundtrip() {
        let db = memory().await.unwrap();

        let mut conn = db.inner.acquire().await.unwrap();
        let mut db_tx = conn.begin().await.unwrap();

        let id = OrderId::default();

        insert_dummy_closed_cfd(&mut db_tx, id).await.unwrap();

        let inserted = Settlement::Refund {
            commit_txid: Txid::new(bdk::bitcoin::Txid::default()),
            txid: Txid::new(bdk::bitcoin::Txid::default()),
            vout: Vout::new(0),
            payout: Payout::new(Amount::ONE_BTC),
        };

        insert_settlement(&mut db_tx, id, inserted).await.unwrap();
        db_tx.commit().await.unwrap();

        let loaded = load_refund_settlement(&mut conn, id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(inserted, loaded);
    }

    async fn insert_dummy_closed_cfd(
        conn: &mut Transaction<'_, Sqlite>,
        id: OrderId,
    ) -> Result<()> {
        let cfd = ClosedCfdInput {
            id,
            position: Position::Long,
            initial_price: Price::new(Decimal::ONE).unwrap(),
            taker_leverage: Leverage::TWO,
            n_contracts: Contracts::new(100),
            counterparty_network_identity: dummy_identity(),
            role: Role::Maker,
            fees: Fees::new(SignedAmount::ONE_BTC),
            expiry_timestamp: OffsetDateTime::now_utc(),
            lock: Lock {
                txid: Txid::new(bdk::bitcoin::Txid::default()),
                dlc_vout: Vout::new(0),
            },
            settlement: Settlement::Collaborative {
                txid: Txid::new(bdk::bitcoin::Txid::default()),
                vout: Vout::new(0),
                payout: Payout::new(Amount::ONE_BTC),
                price: Price::new(Decimal::ONE_HUNDRED).unwrap(),
            },
        };

        insert_closed_cfd(conn, cfd).await?;

        Ok(())
    }

    fn cfd_collaboratively_settled() -> (Cfd, CfdEvent, CfdEvent) {
        // 1|<RANDOM-ORDER-ID>|Long|41772.8325|2|24|100|
        // 69a42aa90da8b065b9532b62bff940a3ba07dbbb11d4482c7db83a7e049a9f1e|Taker|0|0|1
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
            OpeningFee::new(Amount::ZERO),
            FundingRate::default(),
            TxFeeRate::default(),
        );

        let contract_setup_completed =
            std::fs::read_to_string("./src/test_events/contract_setup_completed.json").unwrap();
        let contract_setup_completed =
            serde_json::from_str::<EventKind>(&contract_setup_completed).unwrap();
        let contract_setup_completed = CfdEvent {
            timestamp: Timestamp::now(),
            id: order_id,
            event: contract_setup_completed,
        };

        let collaborative_settlement_completed =
            std::fs::read_to_string("./src/test_events/collaborative_settlement_completed.json")
                .unwrap();
        let collaborative_settlement_completed =
            serde_json::from_str::<EventKind>(&collaborative_settlement_completed).unwrap();
        let collaborative_settlement_completed = CfdEvent {
            timestamp: Timestamp::now(),
            id: order_id,
            event: collaborative_settlement_completed,
        };

        (
            cfd,
            contract_setup_completed,
            collaborative_settlement_completed,
        )
    }

    fn collab_settlement_confirmed(cfd: &Cfd) -> CfdEvent {
        CfdEvent {
            timestamp: Timestamp::now(),
            id: cfd.id(),
            event: EventKind::CollaborativeSettlementConfirmed,
        }
    }

    #[derive(Clone)]
    struct DummyAggregate;

    impl CfdAggregate for DummyAggregate {
        type CtorArgs = ();

        fn new(_: Self::CtorArgs, _: crate::db::Cfd) -> Self {
            Self
        }

        fn apply(self, _: CfdEvent) -> Self {
            Self
        }

        fn version(&self) -> u32 {
            0
        }
    }

    impl ClosedCfdAggregate for DummyAggregate {
        fn new_closed(_: Self::CtorArgs, _: ClosedCfd) -> Self {
            Self
        }
    }

    fn dummy_identity() -> Identity {
        Identity::new(x25519_dalek::PublicKey::from(
            *b"hello world, oh what a beautiful",
        ))
    }
}
