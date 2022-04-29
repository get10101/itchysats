use crate::contract_setup::SetupParams;
use crate::hex_transaction;
use crate::olivia;
use crate::olivia::BitMexPriceEventId;
use crate::payout_curve;
use crate::rollover;
use crate::rollover::RolloverParams;
use crate::FeeAccount;
use crate::FeeFlow;
use crate::FundingFee;
use crate::FundingRate;
use crate::Identity;
use crate::InversePrice;
use crate::Leverage;
use crate::OpeningFee;
use crate::Percent;
use crate::Position;
use crate::Price;
use crate::Timestamp;
use crate::TradingPair;
use crate::TxFeeRate;
use crate::Usd;
use crate::SETTLEMENT_INTERVAL;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use bdk::bitcoin::secp256k1::SecretKey;
use bdk::bitcoin::secp256k1::Signature;
use bdk::bitcoin::Address;
use bdk::bitcoin::Amount;
use bdk::bitcoin::PublicKey;
use bdk::bitcoin::Script;
use bdk::bitcoin::SignedAmount;
use bdk::bitcoin::Transaction;
use bdk::bitcoin::TxIn;
use bdk::bitcoin::TxOut;
use bdk::bitcoin::Txid;
use bdk::descriptor::Descriptor;
use bdk::miniscript::DescriptorTrait;
use cached::proc_macro::cached;
use itertools::Itertools;
use maia::generate_payouts;
use maia::secp256k1_zkp;
use maia::secp256k1_zkp::EcdsaAdaptorSignature;
use maia::secp256k1_zkp::SECP256K1;
use maia::spending_tx_sighash;
use maia::Payout;
use maia::TransactionExt;
use num::Zero;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::de::Error as _;
use serde::Deserialize;
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::ops::RangeInclusive;
use std::str;
use time::Duration;
use time::OffsetDateTime;
use uuid::adapter::Hyphenated;
use uuid::Uuid;

pub const CET_TIMELOCK: u32 = 12;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct OrderId(Hyphenated);

impl Serialize for OrderId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for OrderId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let uuid = String::deserialize(deserializer)?;
        let uuid = uuid.parse::<Uuid>().map_err(D::Error::custom)?;

        Ok(Self(uuid.to_hyphenated()))
    }
}

impl Default for OrderId {
    fn default() -> Self {
        Self(Uuid::new_v4().to_hyphenated())
    }
}

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Uuid> for OrderId {
    fn from(id: Uuid) -> Self {
        OrderId(id.to_hyphenated())
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct MakerOffers {
    pub long: Option<Order>,
    pub short: Option<Order>,
    pub tx_fee_rate: TxFeeRate,
    pub funding_rate_long: FundingRate,
    pub funding_rate_short: FundingRate,
}

impl fmt::Debug for MakerOffers {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("MakerOffers")
            .field("long_order_id", &self.long.as_ref().map(|o| o.id))
            .field("short_order_id", &self.short.as_ref().map(|o| o.id))
            .field("tx_fee_rate", &self.tx_fee_rate)
            .field("funding_rate_long", &self.funding_rate_long)
            .field("funding_rate_short", &self.funding_rate_short)
            .finish()
    }
}

impl MakerOffers {
    /// Picks the order to take if available
    ///
    /// Returns the order to take without removing it.
    pub fn pick_order_to_take(&self, id: OrderId) -> Option<Order> {
        if let Some(long) = self.long {
            if long.id == id {
                return Some(long);
            }
        }
        if let Some(short) = self.short {
            if short.id == id {
                return Some(short);
            }
        }
        None
    }

    /// Takes the order if available
    ///
    /// Resets the order that was taken to None.
    pub fn take_order(mut self, id: OrderId) -> (Option<Order>, Self) {
        if let Some(long) = self.long {
            if long.id == id {
                self.long = None;

                return (Some(long), self);
            }
        }
        if let Some(short) = self.short {
            if short.id == id {
                self.short = None;

                return (Some(short), self);
            }
        }
        (None, self)
    }

    /// Update the orders after one of them got taken.
    pub fn replicate(&self) -> MakerOffers {
        MakerOffers {
            long: self.long.map(|order| order.replicate()),
            short: self.short.map(|order| order.replicate()),
            tx_fee_rate: self.tx_fee_rate,
            funding_rate_long: self.funding_rate_long,
            funding_rate_short: self.funding_rate_short,
        }
    }
}

/// Origin of the order
#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, sqlx::Type)]
pub enum Origin {
    Ours,
    Theirs,
}

/// Role in the Cfd
#[derive(Debug, Copy, Clone, PartialEq, sqlx::Type, Serialize, Deserialize)]
pub enum Role {
    Maker,
    Taker,
}

impl From<Origin> for Role {
    fn from(origin: Origin) -> Self {
        match origin {
            Origin::Ours => Role::Maker,
            Origin::Theirs => Role::Taker,
        }
    }
}

/// A concrete order created by a maker for a taker
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Order {
    pub id: OrderId,

    pub trading_pair: TradingPair,

    /// The maker's position
    ///
    /// Since the maker is the creator of this order this always reflects the maker's position.
    /// When we create a Cfd we change it to be the position as seen by each party, i.e. flip the
    /// position for the taker.
    #[serde(rename = "position")]
    pub position_maker: Position,

    pub price: Price,

    pub min_quantity: Usd,
    pub max_quantity: Usd,

    /// The taker leverage that the maker allows for the taker
    ///
    /// Currently only leverage of x2 is possible, that's why this is not a range of choices.
    /// The maker's leverage always set o x1 is currently just implied.
    #[serde(rename = "leverage")]
    pub leverage_taker: Leverage,

    /// The creation timestamp as set by the maker
    #[serde(rename = "creation_timestamp")]
    pub creation_timestamp_maker: Timestamp,

    /// The duration that will be used for calculating the settlement timestamp
    pub settlement_interval: Duration,

    pub origin: Origin,

    /// The id of the event to be used for price attestation
    ///
    /// The maker includes this into the Order based on the Oracle announcement to be used.
    pub oracle_event_id: BitMexPriceEventId,

    pub tx_fee_rate: TxFeeRate,
    pub funding_rate: FundingRate,
    pub opening_fee: OpeningFee,
}

impl Order {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        position_maker: Position,
        price: Price,
        min_quantity: Usd,
        max_quantity: Usd,
        origin: Origin,
        oracle_event_id: BitMexPriceEventId,
        settlement_interval: Duration,
        tx_fee_rate: TxFeeRate,
        funding_rate: FundingRate,
        opening_fee: OpeningFee,
    ) -> Self {
        let leverage_choices_for_taker = Leverage::TWO;

        Order {
            id: OrderId::default(),
            price,
            min_quantity,
            max_quantity,
            leverage_taker: leverage_choices_for_taker,
            trading_pair: TradingPair::BtcUsd,
            position_maker,
            creation_timestamp_maker: Timestamp::now(),
            settlement_interval,
            origin,
            oracle_event_id,
            tx_fee_rate,
            funding_rate,
            opening_fee,
        }
    }

    /// Replicates the order with a new ID
    pub fn replicate(&self) -> Self {
        Self::new(
            self.position_maker,
            self.price,
            self.min_quantity,
            self.max_quantity,
            self.origin,
            self.oracle_event_id,
            self.settlement_interval,
            self.tx_fee_rate,
            self.funding_rate,
            self.opening_fee,
        )
    }

    /// Defines when we consider an order to be outdated
    ///
    /// If the maker's offer creation timestamp is older than `OUTDATED_AFTER_MINS` minutes then we
    /// consider an order to be outdated.
    const OUTDATED_AFTER_MINS: i64 = 10;

    /// Defines when we consider the order to be outdated.
    ///
    /// This is used as a safety net to prevent the taker from taking an outdated order.
    pub fn is_safe_to_take(&self, now: OffsetDateTime) -> bool {
        !self.is_creation_timestamp_outdated(now) && self.is_oracle_event_timestamp_sane(now)
    }

    /// Check if the the maker's offer creation timestamp is outdated
    ///
    /// If the creation timestamp is older than `OUTDATED_AFTER_MINS` minutes the offer is
    /// considered outdated
    fn is_creation_timestamp_outdated(&self, now: OffsetDateTime) -> bool {
        self.creation_timestamp_maker.seconds() + (Self::OUTDATED_AFTER_MINS * 60)
            < now.unix_timestamp()
    }

    /// Check the oracle event's timestamp for sanity
    ///
    /// An id within [25h, 23h] from now is considered sane.
    fn is_oracle_event_timestamp_sane(&self, now: OffsetDateTime) -> bool {
        let event_id_timestamp = self.oracle_event_id.timestamp();

        let settlement_interval_minus_one_hour = now + SETTLEMENT_INTERVAL - Duration::HOUR;
        let settlement_interval_plus_one_hour = now + SETTLEMENT_INTERVAL + Duration::HOUR;

        event_id_timestamp >= settlement_interval_minus_one_hour
            && event_id_timestamp <= settlement_interval_plus_one_hour
    }
}

/// Proposed collaborative settlement
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct SettlementProposal {
    pub order_id: OrderId,
    pub timestamp: Timestamp,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub taker: Amount,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub maker: Amount,
    pub price: Price,
}

/// Reasons why we cannot rollover a CFD.
#[derive(thiserror::Error, Debug, PartialEq, Clone, Copy)]
pub enum NoRolloverReason {
    #[error("Is too recent to auto-rollover")]
    TooRecent,
    #[error("CFD does not have a DLC")]
    NoDlc,
    #[error("Cannot roll over when CFD not locked yet")]
    NotLocked,
    #[error("Cannot roll over when CFD is committed")]
    Committed,
    #[error("Cannot roll over while CFD is in collaborative settlement")]
    InCollaborativeSettlement,
    #[error("Cannot roll over when CFD is already closed")]
    Closed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CfdEvent {
    pub timestamp: Timestamp,
    pub id: OrderId,
    pub event: EventKind,
}

impl CfdEvent {
    pub fn new(id: OrderId, event: EventKind) -> Self {
        CfdEvent {
            timestamp: Timestamp::now(),
            id,
            event,
        }
    }

    /// Comparison function to order two events chronologically.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use model::{CfdEvent, Timestamp, EventKind, OrderId};
    ///
    /// # let id = OrderId::default();
    /// # let make_event_at = move |seconds| CfdEvent {
    /// #    timestamp: Timestamp::new(seconds),
    /// #    id,
    /// #    event: EventKind::LockConfirmed,
    /// # };
    /// #
    /// let first = make_event_at(2000);
    /// let second = make_event_at(3000);
    ///
    /// let mut events = vec![second.clone(), first.clone()];
    /// events.sort_unstable_by(CfdEvent::chronologically);
    ///
    /// assert_eq!(events, vec![first, second])
    /// ```
    pub fn chronologically(left: &CfdEvent, right: &CfdEvent) -> Ordering {
        left.timestamp.cmp(&right.timestamp)
    }
}

/// Types of events related to a CFD which can be emitted by both
/// maker and taker.
///
/// Unfortunately, despite being a shared type some of the variants
/// are only relevant for specific roles.
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
#[serde(tag = "name", content = "data")]
pub enum EventKind {
    ContractSetupStarted,
    ContractSetupCompleted {
        dlc: Dlc,
    },

    ContractSetupFailed,
    OfferRejected,

    RolloverStarted,
    RolloverAccepted,
    RolloverRejected,
    RolloverCompleted {
        dlc: Dlc,
        funding_fee: FundingFee,
    },
    RolloverFailed,

    CollaborativeSettlementStarted {
        proposal: SettlementProposal,
    },
    CollaborativeSettlementProposalAccepted,
    CollaborativeSettlementCompleted {
        #[serde(with = "hex_transaction")]
        spend_tx: Transaction,
        script: Script,
        price: Price,
    },
    CollaborativeSettlementRejected,
    // TODO: We can distinguish different "failed" scenarios and potentially decide to publish the
    // commit transaction for some
    CollaborativeSettlementFailed,

    LockConfirmed,
    /// The lock transaction is confirmed after CFD was closed
    ///
    /// This can happen in cases where we publish a settlement transaction while the lock
    /// transaction is still pending and they end up in the same block.
    /// We include cases where we already have a transaction spending from lock, but it might not
    /// be final yet.
    LockConfirmedAfterFinality,
    CommitConfirmed,
    CetConfirmed,
    RefundConfirmed,
    RevokeConfirmed,
    CollaborativeSettlementConfirmed,

    CetTimelockExpiredPriorOracleAttestation,
    CetTimelockExpiredPostOracleAttestation {
        #[serde(with = "hex_transaction")]
        cet: Transaction,
    },

    RefundTimelockExpired {
        #[serde(with = "hex_transaction")]
        refund_tx: Transaction,
    },

    OracleAttestedPriorCetTimelock {
        #[serde(with = "hex_transaction")]
        timelocked_cet: Transaction,
        /// The commit transaction for the DLC of this CFD.
        ///
        /// If this is set to `Some`, we haven't previously attempted broadcast `commit_tx` and
        /// need to broadcast it as a result of this event.
        #[serde(with = "hex_transaction::opt")]
        commit_tx: Option<Transaction>,
        price: Price,
    },
    OracleAttestedPostCetTimelock {
        #[serde(with = "hex_transaction")]
        cet: Transaction,
        price: Price,
    },
    ManualCommit {
        #[serde(with = "hex_transaction")]
        tx: Transaction,
    },
}

impl EventKind {
    pub const CONTRACT_SETUP_STARTED: &'static str = "ContractSetupCompleted";
    pub const CONTRACT_SETUP_COMPLETED_EVENT: &'static str = "ContractSetupCompleted";
    pub const ROLLOVER_COMPLETED_EVENT: &'static str = "RolloverCompleted";
    pub const COLLABORATIVE_SETTLEMENT_CONFIRMED: &'static str = "CollaborativeSettlementConfirmed";
    pub const CET_CONFIRMED: &'static str = "CetConfirmed";
    pub const REFUND_CONFIRMED: &'static str = "RefundConfirmed";
    pub const CONTRACT_SETUP_FAILED: &'static str = "ContractSetupFailed";
    pub const OFFER_REJECTED: &'static str = "OfferRejected";

    pub fn to_json(&self) -> (String, String) {
        let value = serde_json::to_value(self).expect("serialization to always work");
        let object = value.as_object().expect("always an object");

        let name = object
            .get("name")
            .expect("to have property `name`")
            .as_str()
            .expect("name to be `string`")
            .to_owned();
        let data = object.get("data").cloned().unwrap_or_default().to_string();

        (name, data)
    }

    pub fn from_json(name: String, data: String) -> Result<Self> {
        match name.as_str() {
            Self::CONTRACT_SETUP_COMPLETED_EVENT | Self::ROLLOVER_COMPLETED_EVENT => {
                from_json_inner_cached(name, data)
            }
            _ => from_json_inner(name, data),
        }
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use EventKind::*;
        let s = match self {
            ContractSetupStarted => "ContractSetupStarted",
            ContractSetupCompleted { .. } => "ContractSetupCompleted",
            ContractSetupFailed => "ContractSetupFailed",
            OfferRejected => "OfferRejected",
            RolloverStarted => "RolloverStarted",
            RolloverAccepted => "RolloverAccepted",
            RolloverRejected => "RolloverRejected",
            RolloverCompleted { .. } => "RolloverCompleted",
            RolloverFailed => "RolloverFailed",
            CollaborativeSettlementStarted { .. } => "CollaborativeSettlementStarted",
            CollaborativeSettlementProposalAccepted => "CollaborativeSettlementProposalAccepted",
            CollaborativeSettlementCompleted { .. } => "CollaborativeSettlementCompleted",
            CollaborativeSettlementRejected => "CollaborativeSettlementRejected",
            CollaborativeSettlementFailed => "CollaborativeSettlementFailed",
            LockConfirmed => "LockConfirmed",
            LockConfirmedAfterFinality => "LockConfirmedAfterFinality",
            CommitConfirmed => "CommitConfirmed",
            CetConfirmed => "CetConfirmed",
            RefundConfirmed => "RefundConfirmed",
            RevokeConfirmed => "RevokeConfirmed",
            CollaborativeSettlementConfirmed => "CollaborativeSettlementConfirmed",
            CetTimelockExpiredPriorOracleAttestation => "CetTimelockExpiredPriorOracleAttestation",
            CetTimelockExpiredPostOracleAttestation { .. } => {
                "CetTimelockExpiredPostOracleAttestation"
            }
            RefundTimelockExpired { .. } => "RefundTimelockExpired",
            OracleAttestedPriorCetTimelock { .. } => "OracleAttestedPriorCetTimelock",
            OracleAttestedPostCetTimelock { .. } => "OracleAttestedPostCetTimelock",
            ManualCommit { .. } => "ManualCommit",
        };

        s.fmt(f)
    }
}

// Deserialisation of events has been proved to use substantial amount of the CPU.
// Cache the events.
#[cached(size = 500, result = true)]
fn from_json_inner_cached(name: String, data: String) -> Result<EventKind> {
    from_json_inner(name, data)
}

fn from_json_inner(name: String, data: String) -> Result<EventKind> {
    use serde_json::json;

    let data = serde_json::from_str::<serde_json::Value>(&data)?;

    let event = serde_json::from_value::<EventKind>(json!({
        "name": name,
        "data": data
    }))?;

    Ok(event)
}

/// Models the cfd state of the taker
///
/// Upon `Command`s, that are reaction to something happening in the system, we decide to
/// produce `Event`s that are saved in the database. After saving an `Event` in the database
/// we apply the event to the aggregate producing a new aggregate (representing the latest state
/// `version`). To bring a cfd into a certain state version we load all events from the
/// database and apply them in order (order by version).
#[derive(Clone, Debug, PartialEq)]
pub struct Cfd {
    version: u32,

    // static
    id: OrderId,
    position: Position,
    initial_price: Price,
    initial_funding_rate: FundingRate,
    long_leverage: Leverage,
    short_leverage: Leverage,
    settlement_interval: Duration,
    quantity: Usd,
    counterparty_network_identity: Identity,
    role: Role,
    opening_fee: OpeningFee,
    initial_tx_fee_rate: TxFeeRate,
    // dynamic (based on events)
    fee_account: FeeAccount,

    dlc: Option<Dlc>,

    /// Holds the decrypted CET transaction if we have previously emitted it as part of an event.
    ///
    /// There is not guarantee that the transaction is confirmed if this is set to `Some`.
    /// However, if this is set to `Some`, there is no need to re-emit it as part of another event.
    cet: Option<Transaction>,

    /// Holds the decrypted commit transaction if we have previously emitted it as part of an
    /// event.
    ///
    /// There is not guarantee that the transaction is confirmed if this is set to `Some`.
    /// However, if this is set to `Some`, there is no need to re-emit it as part of another event.
    commit_tx: Option<Transaction>,

    collaborative_settlement_spend_tx: Option<Transaction>,
    refund_tx: Option<Transaction>,

    lock_finality: bool,

    commit_finality: bool,
    refund_finality: bool,
    cet_finality: bool,
    collaborative_settlement_finality: bool,
    cet_timelock_expired: bool,

    refund_timelock_expired: bool,

    during_contract_setup: bool,
    during_rollover: bool,
    settlement_proposal: Option<SettlementProposal>,
}

impl Cfd {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: OrderId,
        position: Position,
        initial_price: Price,
        taker_leverage: Leverage,
        settlement_interval: Duration, /* TODO: Make a newtype that enforces hours only so
                                        * we don't have to deal with precisions in the
                                        * database. */
        role: Role,
        quantity: Usd,
        counterparty_network_identity: Identity,
        opening_fee: OpeningFee,
        initial_funding_rate: FundingRate,
        initial_tx_fee_rate: TxFeeRate,
    ) -> Self {
        let (long_leverage, short_leverage) =
            long_and_short_leverage(taker_leverage, role, position);

        let initial_funding_fee = FundingFee::calculate(
            initial_price,
            quantity,
            long_leverage,
            short_leverage,
            initial_funding_rate,
            SETTLEMENT_INTERVAL.whole_hours(),
        )
        .expect("values from db to be sane");

        Cfd {
            version: 0,
            id,
            position,
            initial_price,
            long_leverage,
            short_leverage,
            settlement_interval,
            quantity,
            counterparty_network_identity,
            role,
            initial_funding_rate,
            opening_fee,
            initial_tx_fee_rate,
            dlc: None,
            cet: None,
            commit_tx: None,
            collaborative_settlement_spend_tx: None,
            refund_tx: None,
            lock_finality: false,
            commit_finality: false,
            refund_finality: false,
            cet_finality: false,
            collaborative_settlement_finality: false,
            cet_timelock_expired: false,
            refund_timelock_expired: false,
            during_contract_setup: false,
            during_rollover: false,
            settlement_proposal: None,
            fee_account: FeeAccount::new(position, role)
                .add_opening_fee(opening_fee)
                .add_funding_fee(initial_funding_fee),
        }
    }

    /// A convenience method, creating a Cfd from an Order
    pub fn from_order(
        order: Order,
        quantity: Usd,
        counterparty_network_identity: Identity,
        role: Role,
    ) -> Self {
        let position = match role {
            Role::Maker => order.position_maker,
            Role::Taker => order.position_maker.counter_position(),
        };

        Cfd::new(
            order.id,
            position,
            order.price,
            order.leverage_taker,
            order.settlement_interval,
            role,
            quantity,
            counterparty_network_identity,
            order.opening_fee,
            order.funding_rate,
            order.tx_fee_rate,
        )
    }

    fn expiry_timestamp(&self) -> Option<OffsetDateTime> {
        self.dlc
            .as_ref()
            .map(|dlc| dlc.settlement_event_id.timestamp())
    }

    fn margin(&self) -> Amount {
        match self.position {
            Position::Long => {
                calculate_margin(self.initial_price, self.quantity, self.long_leverage)
            }
            Position::Short => {
                calculate_margin(self.initial_price, self.quantity, self.short_leverage)
            }
        }
    }

    fn counterparty_margin(&self) -> Amount {
        match self.position {
            Position::Long => {
                calculate_margin(self.initial_price, self.quantity, self.short_leverage)
            }
            Position::Short => {
                calculate_margin(self.initial_price, self.quantity, self.long_leverage)
            }
        }
    }

    fn is_in_collaborative_settlement(&self) -> bool {
        self.settlement_proposal.is_some()
    }

    fn is_in_force_close(&self) -> bool {
        self.commit_tx.is_some()
    }

    pub fn can_auto_rollover_taker(&self, now: OffsetDateTime) -> Result<(), NoRolloverReason> {
        let expiry_timestamp = self.expiry_timestamp().ok_or(NoRolloverReason::NoDlc)?;
        let time_until_expiry = expiry_timestamp - now;
        if time_until_expiry > SETTLEMENT_INTERVAL - Duration::HOUR {
            return Err(NoRolloverReason::TooRecent);
        }

        self.can_rollover()?;

        Ok(())
    }

    fn can_rollover(&self) -> Result<(), NoRolloverReason> {
        if self.is_closed() {
            return Err(NoRolloverReason::Closed);
        }

        if self.commit_finality {
            return Err(NoRolloverReason::Committed);
        }

        if !self.lock_finality {
            return Err(NoRolloverReason::NotLocked);
        }

        if self.is_in_force_close() {
            return Err(NoRolloverReason::Committed);
        }

        // Rollover and collaborative settlement are mutually exclusive, if we are currently
        // collaboratively settling we cannot roll over
        if self.is_in_collaborative_settlement() {
            return Err(NoRolloverReason::InCollaborativeSettlement);
        }

        Ok(())
    }

    fn can_settle_collaboratively(&self) -> bool {
        !self.is_closed()
            && !self.commit_finality
            && !self.is_attested()
            && !self.is_in_force_close()
    }

    fn is_attested(&self) -> bool {
        self.cet.is_some()
    }

    /// Any transaction spending from lock has reached finality on the blockchain
    fn is_final(&self) -> bool {
        self.collaborative_settlement_finality || self.cet_finality || self.refund_finality
    }

    fn is_collaboratively_closed(&self) -> bool {
        self.collaborative_settlement_spend_tx.is_some()
    }

    fn is_refunded(&self) -> bool {
        self.refund_tx.is_some()
    }

    /// Aggregate that defines if a CFD is considered closed
    ///
    /// A CFD is considered closed when the closing price can't change anymore, which means that we
    /// have either spending transaction set. This is the case if:
    /// - the cfd is already final (early exit if we have already reached finality on the
    ///   blockchain)
    /// - the cfd was attested (i.e.a CET is set)
    /// - the cfd was collaboratively close (i.e. the collab close transaction is set)
    /// - the cfd was refunded (i.e. the refund transaction is set)
    fn is_closed(&self) -> bool {
        self.is_final()
            || self.is_attested()
            || self.is_collaboratively_closed()
            || self.is_refunded()
    }

    pub fn start_contract_setup(&self) -> Result<(CfdEvent, SetupParams, Position)> {
        if self.version > 0 {
            bail!("Start contract not allowed in version {}", self.version)
        }

        let margin = self.margin();
        let counterparty_margin = self.counterparty_margin();

        Ok((
            CfdEvent::new(self.id(), EventKind::ContractSetupStarted),
            SetupParams::new(
                margin,
                counterparty_margin,
                self.counterparty_network_identity,
                self.initial_price,
                self.quantity,
                self.long_leverage,
                self.short_leverage,
                self.refund_timelock_in_blocks(),
                self.initial_tx_fee_rate(),
                self.fee_account,
            )?,
            self.position,
        ))
    }

    pub fn start_rollover(&self) -> Result<CfdEvent> {
        if self.during_rollover {
            bail!("The CFD is already being rolled over")
        };

        self.can_rollover()?;

        Ok(CfdEvent::new(self.id, EventKind::RolloverStarted))
    }

    pub fn accept_rollover_proposal(
        self,
        tx_fee_rate: TxFeeRate,
        funding_rate: FundingRate,
        version: rollover::Version,
    ) -> Result<(CfdEvent, RolloverParams, Dlc, Position, Duration)> {
        if !self.during_rollover {
            bail!("The CFD is not rolling over");
        }

        if self.role != Role::Maker {
            bail!("Can only accept proposal as a maker");
        }

        let hours_to_charge = match version {
            rollover::Version::V1 => 1,
            rollover::Version::V2 => self.hours_to_extend_in_rollover()?,
        };

        let funding_fee = FundingFee::calculate(
            self.initial_price,
            self.quantity,
            self.long_leverage,
            self.short_leverage,
            funding_rate,
            hours_to_charge as i64,
        )?;

        tracing::debug!(
            order_id = %self.id,
            rollover_version = %version,
            %hours_to_charge,
            funding_fee = %funding_fee.compute_relative(self.position),
            "Accepting rollover proposal"
        );

        Ok((
            CfdEvent::new(self.id, EventKind::RolloverAccepted),
            RolloverParams::new(
                self.initial_price,
                self.quantity,
                self.long_leverage,
                self.short_leverage,
                self.refund_timelock_in_blocks(),
                tx_fee_rate,
                self.fee_account,
                funding_fee,
                version,
            ),
            self.dlc.clone().context("No DLC present")?,
            self.position,
            self.settlement_interval,
        ))
    }

    pub fn handle_rollover_accepted_taker(
        &self,
        tx_fee_rate: TxFeeRate,
        funding_rate: FundingRate,
    ) -> Result<(CfdEvent, RolloverParams, Dlc, Position)> {
        if !self.during_rollover {
            bail!("The CFD is not rolling over");
        }

        if self.role != Role::Taker {
            bail!("Can only handle accepted proposal as a taker");
        }

        self.can_rollover()?;

        let hours_to_charge = self.hours_to_extend_in_rollover()?;
        let funding_fee = FundingFee::calculate(
            self.initial_price,
            self.quantity,
            self.long_leverage,
            self.short_leverage,
            funding_rate,
            hours_to_charge as i64,
        )?;

        Ok((
            self.event(EventKind::RolloverAccepted),
            RolloverParams::new(
                self.initial_price,
                self.quantity,
                self.long_leverage,
                self.short_leverage,
                self.refund_timelock_in_blocks(),
                tx_fee_rate,
                self.fee_account,
                funding_fee,
                rollover::Version::V2,
            ),
            self.dlc.clone().context("No DLC present")?,
            self.position,
        ))
    }

    pub fn sign_collaborative_settlement_maker(
        &self,
        proposal: SettlementProposal,
        sig_taker: Signature,
    ) -> Result<CollaborativeSettlement> {
        debug_assert_eq!(
            self.role,
            Role::Maker,
            "Only the maker can complete collaborative settlement signing"
        );

        let dlc = self
            .dlc
            .as_ref()
            .context("Collaborative close without DLC")?;

        let (tx, sig_maker, lock_amount) = dlc.close_transaction(&proposal)?;

        let spend_tx = dlc
            .finalize_spend_transaction(tx, sig_maker, sig_taker, lock_amount)
            .context("Failed to finalize collaborative settlement transaction")?;
        let script_pk = dlc.script_pubkey_for(Role::Maker);

        let settlement = CollaborativeSettlement::new(spend_tx, script_pk, proposal.price)?;
        Ok(settlement)
    }

    pub fn propose_collaborative_settlement(
        &self,
        current_price: Price,
        n_payouts: usize,
    ) -> Result<(CfdEvent, SettlementProposal)> {
        anyhow::ensure!(
            !self.is_in_collaborative_settlement()
                && self.role == Role::Taker
                && self.can_settle_collaboratively(),
            "Failed to propose collaborative settlement"
        );

        let payout_curve = calculate_payouts(
            self.position,
            self.role,
            self.initial_price,
            self.quantity,
            self.long_leverage,
            self.short_leverage,
            n_payouts,
            self.fee_account.settle(),
        )?;

        let payout = {
            let current_price = current_price.try_into_u64()?;
            payout_curve
                .iter()
                .find(|&x| x.digits().range().contains(&current_price))
                .context("find current price on the payout curve")?
        };

        let proposal = SettlementProposal {
            order_id: self.id,
            timestamp: Timestamp::now(),
            taker: *payout.taker_amount(),
            maker: *payout.maker_amount(),
            price: current_price,
        };

        Ok((
            CfdEvent::new(
                self.id,
                EventKind::CollaborativeSettlementStarted { proposal },
            ),
            proposal,
        ))
    }

    pub fn receive_collaborative_settlement_proposal(
        self,
        proposal: SettlementProposal,
        n_payouts: usize,
    ) -> Result<CfdEvent> {
        anyhow::ensure!(
            !self.is_in_collaborative_settlement()
                && self.role == Role::Maker
                && self.can_settle_collaboratively()
                && proposal.order_id == self.id,
            "Failed to start collaborative settlement"
        );

        // Validate that the amounts sent by the taker are sane according to the payout curve

        let payout_curve_long = calculate_payouts(
            self.position,
            self.role,
            self.initial_price,
            self.quantity,
            self.long_leverage,
            self.short_leverage,
            n_payouts,
            self.fee_account.settle(),
        )?;

        let payout = {
            let proposal_price = proposal.price.try_into_u64()?;
            payout_curve_long
                .iter()
                .find(|&x| x.digits().range().contains(&proposal_price))
                .context("find current price on the payout curve")?
        };

        if proposal.maker != *payout.maker_amount() || proposal.taker != *payout.taker_amount() {
            bail!("The settlement amounts sent by the taker are not according to the agreed payout curve. Expected taker {} and maker {} but received taker {} and maker {}", payout.taker_amount(), payout.maker_amount(), proposal.taker, proposal.maker);
        }

        Ok(CfdEvent::new(
            self.id,
            EventKind::CollaborativeSettlementStarted { proposal },
        ))
    }

    pub fn accept_collaborative_settlement_proposal(
        self,
        proposal: &SettlementProposal,
    ) -> Result<CfdEvent> {
        anyhow::ensure!(
            self.role == Role::Maker && self.settlement_proposal.as_ref() == Some(proposal)
        );

        Ok(CfdEvent::new(
            self.id,
            EventKind::CollaborativeSettlementProposalAccepted,
        ))
    }

    pub fn complete_contract_setup(self, dlc: Dlc) -> Result<CfdEvent> {
        if self.version > 1 {
            bail!(
                "Completing contract setup not allowed because cfd in version {}",
                self.version
            )
        }

        tracing::info!(order_id = %self.id, "Contract setup was completed");

        Ok(self.event(EventKind::ContractSetupCompleted { dlc }))
    }

    pub fn reject_contract_setup(self, reason: anyhow::Error) -> Result<CfdEvent> {
        anyhow::ensure!(
            self.version <= 1,
            "Rejecting contract setup not allowed because cfd in version {}",
            self.version
        );

        tracing::info!(order_id = %self.id, "Contract setup was rejected: {reason:#}");

        Ok(self.event(EventKind::OfferRejected))
    }

    pub fn fail_contract_setup(self, error: anyhow::Error) -> CfdEvent {
        tracing::error!(order_id = %self.id, "Contract setup failed: {error:#}");

        self.event(EventKind::ContractSetupFailed)
    }

    pub fn complete_rollover(self, dlc: Dlc, funding_fee: FundingFee) -> CfdEvent {
        match self.can_rollover() {
            Ok(_) => {
                tracing::info!(order_id = %self.id, "Rollover was completed");

                self.event(EventKind::RolloverCompleted { dlc, funding_fee })
            }
            Err(e) => self.fail_rollover(e.into()),
        }
    }

    pub fn reject_rollover(self, reason: anyhow::Error) -> CfdEvent {
        tracing::info!(order_id = %self.id, "Rollover was rejected: {:#}", reason);

        self.event(EventKind::RolloverRejected)
    }

    pub fn fail_rollover(self, error: anyhow::Error) -> CfdEvent {
        tracing::warn!(order_id = %self.id, "Rollover failed: {:#}", error);

        self.event(EventKind::RolloverFailed)
    }

    pub fn complete_collaborative_settlement(
        self,
        settlement: CollaborativeSettlement,
    ) -> CfdEvent {
        if self.can_settle_collaboratively() {
            tracing::info!(order_id=%self.id(), tx=%settlement.tx.txid(), "Collaborative settlement completed");

            self.event(EventKind::CollaborativeSettlementCompleted {
                spend_tx: settlement.tx,
                script: settlement.script_pubkey,
                price: settlement.price,
            })
        } else {
            self.fail_collaborative_settlement(anyhow!("Cannot complete collaborative settlement"))
        }
    }

    pub fn reject_collaborative_settlement(self, reason: anyhow::Error) -> CfdEvent {
        tracing::info!(order_id=%self.id(), "Collaborative settlement rejected: {reason:#}");

        self.event(EventKind::CollaborativeSettlementRejected)
    }

    pub fn fail_collaborative_settlement(self, error: anyhow::Error) -> CfdEvent {
        tracing::warn!(order_id=%self.id(), "Collaborative settlement failed: {:#}", error);

        self.event(EventKind::CollaborativeSettlementFailed)
    }

    /// Given an attestation, find and decrypt the relevant CET.
    ///
    /// In case the Cfd was already closed we return `Ok(None)`, because then the attestation is not
    /// relevant anymore. We don't treat this as error because it is not an error scenario.
    pub fn decrypt_cet(self, attestation: &olivia::Attestation) -> Result<Option<CfdEvent>> {
        if self.is_closed() {
            return Ok(None);
        }

        let dlc = match self.dlc.as_ref() {
            Some(dlc) => dlc,
            None => return Ok(None),
        };

        let cet = dlc.signed_cet(attestation)?;

        let cet = match cet {
            Ok(cet) => cet,
            Err(IrrelevantAttestation { .. }) => {
                return Ok(None);
            }
        };

        let price = Price(Decimal::from(attestation.price));

        if self.cet_timelock_expired {
            return Ok(Some(
                self.event(EventKind::OracleAttestedPostCetTimelock { cet, price }),
            ));
        }

        // If we haven't yet emitted the commit tx, we need to emit it now.
        let commit_tx_to_emit = match self.commit_tx {
            Some(_) => None,
            None => Some(dlc.signed_commit_tx()?),
        };

        Ok(Some(self.event(
            EventKind::OracleAttestedPriorCetTimelock {
                timelocked_cet: cet,
                commit_tx: commit_tx_to_emit,
                price,
            },
        )))
    }

    pub fn handle_cet_timelock_expired(self) -> Result<CfdEvent> {
        anyhow::ensure!(!self.is_final());

        let cfd_event = self
            .cet
            .clone()
            // If we have cet, that means it has been attested
            .map(|cet| EventKind::CetTimelockExpiredPostOracleAttestation { cet })
            .unwrap_or_else(|| EventKind::CetTimelockExpiredPriorOracleAttestation);

        Ok(self.event(cfd_event))
    }

    pub fn handle_refund_timelock_expired(self) -> Result<Option<CfdEvent>> {
        if self.is_closed() {
            return Ok(None);
        }

        let dlc = self.dlc.as_ref().context("CFD does not have a DLC")?;
        let refund_tx = dlc
            .signed_refund_tx()
            .context("Failed to sign refund transaction")?;

        let event = self.event(EventKind::RefundTimelockExpired { refund_tx });

        Ok(Some(event))
    }

    pub fn handle_lock_confirmed(self) -> CfdEvent {
        // For the special case where we close when lock is still pending
        if self.is_closed() || self.is_in_force_close() {
            return self.event(EventKind::LockConfirmedAfterFinality);
        }

        self.event(EventKind::LockConfirmed)
    }

    pub fn handle_commit_confirmed(self) -> CfdEvent {
        self.event(EventKind::CommitConfirmed)
    }

    pub fn handle_collaborative_settlement_confirmed(self) -> CfdEvent {
        self.event(EventKind::CollaborativeSettlementConfirmed)
    }

    pub fn handle_cet_confirmed(self) -> CfdEvent {
        self.event(EventKind::CetConfirmed)
    }

    pub fn handle_refund_confirmed(self) -> CfdEvent {
        tracing::info!(order_id=%self.id, "Refund transaction confirmed");

        self.event(EventKind::RefundConfirmed)
    }

    pub fn handle_revoke_confirmed(self) -> CfdEvent {
        self.event(EventKind::RevokeConfirmed)
    }

    pub fn manual_commit_to_blockchain(&self) -> Result<CfdEvent> {
        anyhow::ensure!(!self.is_closed());

        let dlc = self.dlc.as_ref().context("Cannot commit without a DLC")?;

        Ok(self.event(EventKind::ManualCommit {
            tx: dlc.signed_commit_tx()?,
        }))
    }

    fn event(&self, event: EventKind) -> CfdEvent {
        CfdEvent::new(self.id, event)
    }

    /// A factor to be added to the CFD order settlement_interval for calculating the
    /// refund timelock.
    ///
    /// The refund timelock is important in case the oracle disappears or never publishes a
    /// signature. Ideally, both users collaboratively settle in the refund scenario. This
    /// factor is important if the users do not settle collaboratively.
    /// `1.5` times the settlement_interval as defined in CFD order should be safe in the
    /// extreme case where a user publishes the commit transaction right after the contract was
    /// initialized. In this case, the oracle still has `1.0 *
    /// cfdorder.settlement_interval` time to attest and no one can publish the refund
    /// transaction.
    /// The downside is that if the oracle disappears: the users would only notice at the end
    /// of the cfd settlement_interval. In this case the users has to wait for another
    /// `1.5` times of the settlement_interval to get his funds back.
    const REFUND_THRESHOLD: f32 = 1.5;

    fn refund_timelock_in_blocks(&self) -> u32 {
        (self.settlement_interval * Self::REFUND_THRESHOLD)
            .as_blocks()
            .ceil() as u32
    }

    pub fn id(&self) -> OrderId {
        self.id
    }

    pub fn position(&self) -> Position {
        self.position
    }

    pub fn initial_price(&self) -> Price {
        self.initial_price
    }

    pub fn taker_leverage(&self) -> Leverage {
        match (self.role, self.position) {
            (Role::Taker, Position::Long) | (Role::Maker, Position::Short) => self.long_leverage,
            (Role::Taker, Position::Short) | (Role::Maker, Position::Long) => self.short_leverage,
        }
    }

    pub fn settlement_time_interval_hours(&self) -> Duration {
        self.settlement_interval
    }

    pub fn quantity(&self) -> Usd {
        self.quantity
    }

    pub fn counterparty_network_identity(&self) -> Identity {
        self.counterparty_network_identity
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn initial_funding_rate(&self) -> FundingRate {
        self.initial_funding_rate
    }

    pub fn initial_tx_fee_rate(&self) -> TxFeeRate {
        self.initial_tx_fee_rate
    }

    pub fn opening_fee(&self) -> OpeningFee {
        self.opening_fee
    }

    pub fn sign_collaborative_settlement_taker(
        &self,
        proposal: &SettlementProposal,
    ) -> Result<(Transaction, Signature, Script)> {
        debug_assert_eq!(
            self.role,
            Role::Taker,
            "Only the taker can start collaborative settlement signing"
        );

        let dlc = self
            .dlc
            .as_ref()
            .context("Collaborative close without DLC")?;

        let (tx, sig, _) = dlc.close_transaction(proposal)?;
        let script_pk = dlc.script_pubkey_for(Role::Taker);

        Ok((tx, sig, script_pk))
    }

    /// Number of hours that the time-to-live of the contract will be
    /// extended by with the next rollover.
    ///
    /// During rollover the time-to-live of the contract is extended
    /// so that the non-collaborative settlement time is set to ~24
    /// hours in the future from now.
    fn hours_to_extend_in_rollover(&self) -> Result<u64> {
        let dlc = self.dlc.as_ref().context("Cannot roll over without DLC")?;
        let settlement_time = dlc.settlement_event_id.timestamp();

        let hours_left = settlement_time - OffsetDateTime::now_utc();

        if !hours_left.is_positive() {
            tracing::warn!("Rolling over a contract that can be settled non-collaboratively");

            return Ok(SETTLEMENT_INTERVAL.whole_hours() as u64);
        }

        let time_to_extend = SETTLEMENT_INTERVAL
            .checked_sub(hours_left)
            .context("Subtraction overflow")?;
        let hours_to_extend = time_to_extend.whole_hours();

        if hours_to_extend.is_negative() {
            bail!(
                "Cannot rollover if time-to-live of contract is > {} hours",
                SETTLEMENT_INTERVAL.whole_hours()
            );
        }

        Ok(if hours_to_extend.is_zero() {
            1
        } else {
            hours_to_extend as u64
        })
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn apply(mut self, evt: CfdEvent) -> Cfd {
        use EventKind::*;

        self.version += 1;

        match evt.event {
            ContractSetupStarted => self.during_contract_setup = true,
            ContractSetupCompleted { dlc } => {
                self.dlc = Some(dlc);
                self.during_contract_setup = false;
            }
            OracleAttestedPostCetTimelock { cet, .. } => self.cet = Some(cet),
            OracleAttestedPriorCetTimelock {
                timelocked_cet,
                commit_tx,
                ..
            } => {
                self.cet = Some(timelocked_cet);
                if self.commit_tx.is_none() {
                    self.commit_tx = commit_tx;
                }
            }
            ContractSetupFailed { .. } => {
                self.during_contract_setup = false;
            }
            RolloverStarted => {
                self.during_rollover = true;
            }
            RolloverAccepted => {}
            RolloverCompleted { dlc, funding_fee } => {
                self.dlc = Some(dlc);
                self.during_rollover = false;
                self.fee_account = self.fee_account.add_funding_fee(funding_fee);
            }
            RolloverFailed { .. } => {
                self.during_rollover = false;
            }
            RolloverRejected => {
                self.during_rollover = false;
            }

            CollaborativeSettlementStarted { proposal } => {
                self.settlement_proposal = Some(proposal)
            }
            CollaborativeSettlementProposalAccepted { .. } => {}
            CollaborativeSettlementCompleted { spend_tx, .. } => {
                self.settlement_proposal = None;
                self.collaborative_settlement_spend_tx = Some(spend_tx);
            }
            CollaborativeSettlementRejected | CollaborativeSettlementFailed => {
                self.settlement_proposal = None;
            }
            CetConfirmed => self.cet_finality = true,
            RefundConfirmed => self.refund_finality = true,
            CollaborativeSettlementConfirmed => self.collaborative_settlement_finality = true,
            RefundTimelockExpired { .. } => self.refund_timelock_expired = true,
            LockConfirmed => self.lock_finality = true,
            LockConfirmedAfterFinality => self.lock_finality = true,
            CommitConfirmed => self.commit_finality = true,
            CetTimelockExpiredPriorOracleAttestation
            | CetTimelockExpiredPostOracleAttestation { .. } => {
                self.cet_timelock_expired = true;
            }
            OfferRejected => {
                // nothing to do here? A rejection means it should be impossible to issue any
                // commands
            }
            ManualCommit { tx } => self.commit_tx = Some(tx),
            RevokeConfirmed => {
                tracing::error!(order_id = %self.id, "Revoked logic not implemented");
                // TODO: we should punish the other party instead. For now, we pretend we are in
                // commit finalized and will receive our money based on an old CET.
                self.commit_finality = true;
            }
        }

        self
    }
}

pub trait AsBlocks {
    /// Calculates the duration in Bitcoin blocks.
    ///
    /// On Bitcoin there is a block every 10 minutes/600 seconds on average.
    /// It's the caller's responsibility to round the resulting floating point number.
    fn as_blocks(&self) -> f32;
}

impl AsBlocks for Duration {
    fn as_blocks(&self) -> f32 {
        self.as_seconds_f32() / 60.0 / 10.0
    }
}

/// Determine the leverage based on role and position
pub fn long_and_short_leverage(
    taker_leverage: Leverage,
    role: Role,
    position: Position,
) -> (Leverage, Leverage) {
    match (role, position) {
        (Role::Maker, Position::Long) | (Role::Taker, Position::Short) => {
            (Leverage::ONE, taker_leverage)
        }
        (Role::Maker, Position::Short) | (Role::Taker, Position::Long) => {
            (taker_leverage, Leverage::ONE)
        }
    }
}

/// Calculate the closing price used to collaboratively settle a CFD.
/// This value is akin to the one used for a market close order in a
/// centralised exchange.
///
/// We calculate it from the perspective of the maker (i.e. the market).
///
/// If the maker has gone long, when closing their position they sell
/// short. Therefore, in that case we use the ask price.
///
/// If the maker has gone short, when closing their position they buy
/// long. Therefore, in that case we use the bid price.
pub fn market_closing_price(bid: Price, ask: Price, role: Role, position: Position) -> Price {
    let maker_position = match (role, position) {
        (Role::Maker, maker_position) => maker_position,
        (Role::Taker, taker_position) => taker_position.counter_position(),
    };

    match maker_position {
        Position::Long => ask,
        Position::Short => bid,
    }
}

/// Calculates the margin in BTC
///
/// The initial margin represents the collateral both parties have to come up with
/// to satisfy the contract.
pub fn calculate_margin(price: Price, quantity: Usd, leverage: Leverage) -> Amount {
    quantity / (price * leverage)
}

pub fn calculate_long_liquidation_price(leverage: Leverage, price: Price) -> Price {
    price * leverage / (leverage + 1)
}

/// calculates short liquidation price
///
/// Note: if leverage == 1, then the liquidation price will go towards infinity.
/// This is represented as Price::INFINITE
pub fn calculate_short_liquidation_price(leverage: Leverage, price: Price) -> Price {
    if leverage == Leverage::ONE {
        return Price::INFINITE;
    }
    price * leverage / (leverage - 1)
}

pub fn calculate_profit(payout: SignedAmount, margin: SignedAmount) -> (SignedAmount, Percent) {
    let profit = payout - margin;

    let profit_sats = Decimal::from(profit.as_sat());
    let margin_sats = Decimal::from(margin.as_sat());
    let percent = dec!(100) * profit_sats / margin_sats;

    (profit, Percent(percent))
}

/// Returns the profit/loss and payout capped by the provided margin
///
/// All values are calculated without using the payout curve.
/// Profit/loss is returned as signed bitcoin amount and percent.
pub fn calculate_profit_at_price(
    opening_price: Price,
    closing_price: Price,
    quantity: Usd,
    long_leverage: Leverage,
    short_leverage: Leverage,
    fee_account: FeeAccount,
) -> Result<(SignedAmount, Percent, SignedAmount)> {
    let inv_initial_price =
        InversePrice::new(opening_price).context("cannot invert invalid price")?;
    let inv_closing_price =
        InversePrice::new(closing_price).context("cannot invert invalid price")?;
    let long_liquidation_price = calculate_long_liquidation_price(long_leverage, opening_price);
    let long_is_liquidated = closing_price <= long_liquidation_price;

    let amount_changed = (quantity * inv_initial_price)
        .to_signed()
        .context("Unable to convert to SignedAmount")?
        - (quantity * inv_closing_price)
            .to_signed()
            .context("Unable to convert to SignedAmount")?;

    // calculate profit/loss (P and L) in BTC
    let (margin, payout) = match fee_account.position {
        // TODO: Make sure that what is written down below makes sense (we have the general case
        // now)

        // The general case is:
        //   let:
        //     P = payout
        //     Q = quantity
        //     Ll = long_leverage
        //     Ls = short_leverage
        //     xi = initial_price
        //     xc = closing_price
        //
        //     a = xi * Ll / (Ll + 1)
        //     b = xi * Ls / (Ls - 1)
        //
        //     P_long(xc) = {
        //          0 if xc <= a,
        //          Q / (xi * Ll) + Q * (1 / xi - 1 / xc) if a < xc < b,
        //          Q / xi * (1/Ll + 1/Ls) if xc if xc >= b
        //     }
        //
        //     P_short(xc) = {
        //          Q / xi * (1/Ll + 1/Ls) if xc <= a,
        //          Q / (xi * Ls) - Q * (1 / xi - 1 / xc) if a < xc < b,
        //          0 if xc >= b
        //     }
        Position::Long => {
            let long_margin = calculate_margin(opening_price, quantity, long_leverage)
                .to_signed()
                .context("Unable to compute long margin")?;

            let payout = match long_is_liquidated {
                true => SignedAmount::ZERO,
                false => long_margin + amount_changed - fee_account.balance(),
            };
            (long_margin, payout)
        }
        Position::Short => {
            let long_margin = calculate_margin(opening_price, quantity, long_leverage)
                .to_signed()
                .context("Unable to compute long margin")?;
            let short_margin = calculate_margin(opening_price, quantity, short_leverage)
                .to_signed()
                .context("Unable to compute long margin")?;

            let payout = match long_is_liquidated {
                true => long_margin + short_margin,
                false => short_margin - amount_changed - fee_account.balance(),
            };
            (short_margin, payout)
        }
    };

    let (profit_btc, profit_percent) = calculate_profit(payout, margin);
    Ok((profit_btc, profit_percent, payout))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cet {
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_sat")]
    pub maker_amount: Amount,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_sat")]
    pub taker_amount: Amount,
    pub adaptor_sig: EcdsaAdaptorSignature,

    // TODO: Range + number of digits (usize) could be represented as Digits similar to what we do
    // in the protocol lib
    pub range: RangeInclusive<u64>,
    pub n_bits: usize,

    pub txid: Txid,
}

impl Cet {
    /// Build an actual `Transaction` out of the payout information
    /// stored in `Self`, together with the input and the output
    /// addresses.
    ///
    /// The order of the outputs matters.
    ///
    /// We verify that the TXID of the resulting transaction matches
    /// the TXID with which `Self` was constructed.
    pub fn to_tx(
        &self,
        (commit_tx, commit_descriptor): (&Transaction, &Descriptor<PublicKey>),
        maker_address: &Address,
        taker_address: &Address,
    ) -> Result<Transaction> {
        let tx = Transaction {
            version: 2,
            input: vec![TxIn {
                previous_output: commit_tx.outpoint(&commit_descriptor.script_pubkey())?,
                sequence: CET_TIMELOCK,
                ..Default::default()
            }],
            lock_time: 0,
            output: vec![
                TxOut {
                    value: self.maker_amount.as_sat(),
                    script_pubkey: maker_address.script_pubkey(),
                },
                TxOut {
                    value: self.taker_amount.as_sat(),
                    script_pubkey: taker_address.script_pubkey(),
                },
            ],
        };

        if tx.txid() != self.txid {
            bail!("Reconstructed wrong CET");
        }

        Ok(tx)
    }
}

/// Contains all data we've assembled about the CFD through the setup protocol.
///
/// All contained signatures are the signatures of THE OTHER PARTY.
/// To use any of these transactions, we need to re-sign them with the correct secret key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Dlc {
    pub identity: SecretKey,
    pub identity_counterparty: PublicKey,
    pub revocation: SecretKey,
    pub revocation_pk_counterparty: PublicKey,
    pub publish: SecretKey,
    pub publish_pk_counterparty: PublicKey,
    pub maker_address: Address,
    pub taker_address: Address,

    /// The fully signed lock transaction ready to be published on chain
    pub lock: (Transaction, Descriptor<PublicKey>),
    pub commit: (Transaction, EcdsaAdaptorSignature, Descriptor<PublicKey>),
    pub cets: HashMap<BitMexPriceEventId, Vec<Cet>>,
    pub refund: (Transaction, Signature),

    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_sat")]
    pub maker_lock_amount: Amount,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_sat")]
    pub taker_lock_amount: Amount,

    pub revoked_commit: Vec<RevokedCommit>,

    // TODO: For now we store this seperately - it is a duplicate of what is stored in the cets
    // hashmap. The cet hashmap allows storing cets for event-ids with different concern
    // (settlement and liquidation-point). We should NOT make these fields public on the Dlc
    // and create an internal structure that depicts this properly and avoids duplication.
    pub settlement_event_id: BitMexPriceEventId,
    pub refund_timelock: u32,
}

impl Dlc {
    /// Create a close transaction based on the current contract and a settlement proposals
    pub fn close_transaction(
        &self,
        proposal: &SettlementProposal,
    ) -> Result<(Transaction, Signature, Amount)> {
        let (lock_tx, lock_desc) = &self.lock;
        let (lock_outpoint, lock_amount) = {
            let outpoint = lock_tx
                .outpoint(&lock_desc.script_pubkey())
                .expect("lock script to be in lock tx");
            let amount = Amount::from_sat(lock_tx.output[outpoint.vout as usize].value);

            (outpoint, amount)
        };
        let (tx, sighash) = maia::close_transaction(
            lock_desc,
            lock_outpoint,
            lock_amount,
            (&self.maker_address, proposal.maker),
            (&self.taker_address, proposal.taker),
            1,
        )
        .context("Unable to build collaborative close transaction")?;

        let sig = SECP256K1.sign(&sighash, &self.identity);

        Ok((tx, sig, lock_amount))
    }

    pub fn finalize_spend_transaction(
        &self,
        spend_tx: Transaction,
        own_sig: Signature,
        counterparty_sig: Signature,
        lock_amount: Amount,
    ) -> Result<Transaction> {
        let sighash = spending_tx_sighash(&spend_tx, &self.lock.1, lock_amount);
        SECP256K1
            .verify(&sighash, &counterparty_sig, &self.identity_counterparty.key)
            .context("Failed to verify counterparty signature")?;

        let own_pk = PublicKey::new(secp256k1_zkp::PublicKey::from_secret_key(
            SECP256K1,
            &self.identity,
        ));
        let counterparty_pk = self.identity_counterparty;

        let lock_desc = &self.lock.1;
        let spend_tx = maia::finalize_spend_transaction(
            spend_tx,
            lock_desc,
            (own_pk, own_sig),
            (counterparty_pk, counterparty_sig),
        )?;

        Ok(spend_tx)
    }

    pub fn script_pubkey_for(&self, role: Role) -> Script {
        match role {
            Role::Maker => self.maker_address.script_pubkey(),
            Role::Taker => self.taker_address.script_pubkey(),
        }
    }

    pub fn signed_refund_tx(&self) -> Result<Transaction> {
        let sig_hash = spending_tx_sighash(
            &self.refund.0,
            &self.commit.2,
            Amount::from_sat(self.commit.0.output[0].value),
        );
        let our_sig = SECP256K1.sign(&sig_hash, &self.identity);
        let our_pubkey = PublicKey::new(bdk::bitcoin::secp256k1::PublicKey::from_secret_key(
            SECP256K1,
            &self.identity,
        ));
        let counterparty_sig = self.refund.1;
        let counterparty_pubkey = self.identity_counterparty;
        let signed_refund_tx = maia::finalize_spend_transaction(
            self.refund.0.clone(),
            &self.commit.2,
            (our_pubkey, our_sig),
            (counterparty_pubkey, counterparty_sig),
        )?;

        Ok(signed_refund_tx)
    }

    pub fn signed_commit_tx(&self) -> Result<Transaction> {
        let sig_hash = spending_tx_sighash(
            &self.commit.0,
            &self.lock.1,
            Amount::from_sat(self.lock.0.output[0].value),
        );
        let our_sig = SECP256K1.sign(&sig_hash, &self.identity);
        let our_pubkey = PublicKey::new(bdk::bitcoin::secp256k1::PublicKey::from_secret_key(
            SECP256K1,
            &self.identity,
        ));

        let counterparty_sig = self.commit.1.decrypt(&self.publish)?;
        let counterparty_pubkey = self.identity_counterparty;

        let signed_commit_tx = maia::finalize_spend_transaction(
            self.commit.0.clone(),
            &self.lock.1,
            (our_pubkey, our_sig),
            (counterparty_pubkey, counterparty_sig),
        )?;

        Ok(signed_commit_tx)
    }

    pub fn signed_cet(
        &self,
        attestation: &olivia::Attestation,
    ) -> Result<Result<Transaction, IrrelevantAttestation>> {
        let cets = match self.cets.get(&attestation.id) {
            Some(cets) => cets,
            None => {
                return Ok(Err(IrrelevantAttestation {
                    id: attestation.id,
                    tx_id: self.lock.0.txid(),
                }))
            }
        };

        let cet = cets
            .iter()
            .find(|Cet { range, .. }| range.contains(&attestation.price))
            .context("Price out of range of cets")?;
        let encsig = cet.adaptor_sig;

        let mut decryption_sk = attestation.scalars[0];
        for oracle_attestation in attestation.scalars[1..cet.n_bits].iter() {
            decryption_sk.add_assign(oracle_attestation.as_ref())?;
        }

        let cet = cet
            .to_tx(
                (&self.commit.0, &self.commit.2),
                &self.maker_address,
                &self.taker_address,
            )
            .context("Failed to reconstruct CET")?;

        let sig_hash = spending_tx_sighash(
            &cet,
            &self.commit.2,
            Amount::from_sat(self.commit.0.output[0].value),
        );
        let our_sig = SECP256K1.sign(&sig_hash, &self.identity);
        let our_pubkey = PublicKey::new(bdk::bitcoin::secp256k1::PublicKey::from_secret_key(
            SECP256K1,
            &self.identity,
        ));

        let counterparty_sig = encsig.decrypt(&decryption_sk)?;
        let counterparty_pubkey = self.identity_counterparty;

        let signed_cet = maia::finalize_spend_transaction(
            cet,
            &self.commit.2,
            (our_pubkey, our_sig),
            (counterparty_pubkey, counterparty_sig),
        )?;

        Ok(Ok(signed_cet))
    }
}

#[derive(Debug, thiserror::Error, Clone, Copy)]
#[error("Attestation {id} is irrelevant for DLC {tx_id}")]
pub struct IrrelevantAttestation {
    id: BitMexPriceEventId,
    tx_id: Txid,
}

/// Information which we need to remember in order to construct a
/// punishment transaction in case the counterparty publishes a
/// revoked commit transaction.
///
/// It also includes the information needed to monitor for the
/// publication of the revoked commit transaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokedCommit {
    // To build punish transaction
    pub encsig_ours: EcdsaAdaptorSignature,
    pub revocation_sk_theirs: SecretKey,
    pub publication_pk_theirs: PublicKey,
    // To monitor revoked commit transaction
    pub txid: Txid,
    pub script_pubkey: Script,
}

/// Used when transactions (e.g. collaborative close) are recorded as a part of
/// CfdState in the cases when we can't solely rely on state transition
/// timestamp as it could have occured for different reasons (like a new
/// attestation in Open state)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CollaborativeSettlement {
    pub tx: Transaction,
    pub script_pubkey: Script,
    pub timestamp: Timestamp,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_sat")]
    payout: Amount,
    price: Price,
}

impl CollaborativeSettlement {
    pub fn new(tx: Transaction, own_script_pubkey: Script, price: Price) -> Result<Self> {
        // Falls back to Amount::ZERO in case we don't find an output that matches out script pubkey
        // The assumption is, that this can happen for cases where we were liquidated
        let payout = match tx
            .output
            .iter()
            .find(|output| output.script_pubkey == own_script_pubkey)
            .map(|output| Amount::from_sat(output.value))
        {
            Some(payout) => payout,
            None => {
                tracing::error!(
                    "Collaborative settlement with a zero amount, this should really not happen!"
                );
                Amount::ZERO
            }
        };

        Ok(Self {
            tx,
            script_pubkey: own_script_pubkey,
            timestamp: Timestamp::now(),
            payout,
            price,
        })
    }

    pub fn payout(&self) -> Amount {
        self.payout
    }
}

#[allow(clippy::too_many_arguments)]
pub fn calculate_payouts(
    position: Position,
    role: Role,
    price: Price,
    quantity: Usd,
    long_leverage: Leverage,
    short_leverage: Leverage,
    n_payouts: usize,
    fee: FeeFlow,
) -> Result<Vec<Payout>> {
    let payouts = payout_curve::calculate(
        price,
        quantity,
        long_leverage,
        short_leverage,
        n_payouts,
        fee,
    )?;

    match (position, role) {
        (Position::Long, Role::Taker) | (Position::Short, Role::Maker) => payouts
            .into_iter()
            .map(|payout| generate_payouts(payout.range, payout.short, payout.long))
            .flatten_ok()
            .collect(),
        (Position::Short, Role::Taker) | (Position::Long, Role::Maker) => payouts
            .into_iter()
            .map(|payout| generate_payouts(payout.range, payout.long, payout.short))
            .flatten_ok()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdk::bitcoin;
    use bdk::bitcoin::util::psbt::Global;
    use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
    use bdk_ext::keypair;
    use bdk_ext::SecretKeyExt;
    use maia::lock_descriptor;
    use proptest::prelude::*;
    use rand::thread_rng;
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;
    use std::str::FromStr;
    use time::ext::NumericalDuration;
    use time::macros::datetime;

    #[test]
    fn given_default_values_then_expected_liquidation_price() {
        let price = Price::new(dec!(46125)).unwrap();
        let leverage = Leverage::new(5).unwrap();
        let expected = Price::new(dec!(38437.5)).unwrap();

        let liquidation_price = calculate_long_liquidation_price(leverage, price);

        assert_eq!(liquidation_price, expected);
    }

    #[test]
    fn given_leverage_of_one_and_equal_price_and_quantity_then_long_margin_is_one_btc() {
        let price = Price::new(dec!(40000)).unwrap();
        let quantity = Usd::new(dec!(40000));
        let leverage = Leverage::new(1).unwrap();

        let long_margin = calculate_margin(price, quantity, leverage);

        assert_eq!(long_margin, Amount::ONE_BTC);
    }

    #[test]
    fn given_leverage_of_one_and_leverage_of_ten_then_long_margin_is_lower_factor_ten() {
        let price = Price::new(dec!(40000)).unwrap();
        let quantity = Usd::new(dec!(40000));
        let leverage = Leverage::new(10).unwrap();

        let long_margin = calculate_margin(price, quantity, leverage);

        assert_eq!(long_margin, Amount::from_btc(0.1).unwrap());
    }

    // TODO: These tests need better naming, because the constraint "short is always leverage 1" is
    // gone!
    #[test]
    fn given_quantity_equals_price_then_short_margin_is_one_btc() {
        let price = Price::new(dec!(40000)).unwrap();
        let quantity = Usd::new(dec!(40000));

        let short_margin = calculate_margin(price, quantity, Leverage::ONE);

        assert_eq!(short_margin, Amount::ONE_BTC);
    }

    #[test]
    fn given_quantity_half_of_price_then_short_margin_is_half_btc() {
        let price = Price::new(dec!(40000)).unwrap();
        let quantity = Usd::new(dec!(20000));

        let short_margin = calculate_margin(price, quantity, Leverage::ONE);

        assert_eq!(short_margin, Amount::from_btc(0.5).unwrap());
    }

    #[test]
    fn given_quantity_double_of_price_then_short_margin_is_two_btc() {
        let price = Price::new(dec!(40000)).unwrap();
        let quantity = Usd::new(dec!(80000));

        let short_margin = calculate_margin(price, quantity, Leverage::ONE);

        assert_eq!(short_margin, Amount::from_btc(2.0).unwrap());
    }

    #[test]
    fn test_secs_into_blocks() {
        let error_margin = f32::EPSILON;

        let duration = Duration::seconds(600);
        let blocks = duration.as_blocks();
        assert!(blocks - error_margin < 1.0 && blocks + error_margin > 1.0);

        let duration = Duration::seconds(0);
        let blocks = duration.as_blocks();
        assert!(blocks - error_margin < 0.0 && blocks + error_margin > 0.0);

        let duration = Duration::seconds(60);
        let blocks = duration.as_blocks();
        assert!(blocks - error_margin < 0.1 && blocks + error_margin > 0.1);
    }

    #[test]
    fn calculate_profit_and_loss() {
        let empty_fee_long = FeeAccount::new(Position::Long, Role::Taker);
        let empty_fee_short = FeeAccount::new(Position::Short, Role::Maker);

        assert_profit_loss_values(
            Price::new(dec!(10_000)).unwrap(),
            Price::new(dec!(10_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::TWO,
            Leverage::ONE,
            empty_fee_long,
            SignedAmount::ZERO,
            Decimal::ZERO.into(),
            "No price increase means no profit",
        );

        assert_profit_loss_values(
            Price::new(dec!(10_000)).unwrap(),
            Price::new(dec!(10_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::TWO,
            Leverage::ONE,
            empty_fee_long.add_funding_fee(FundingFee::new(
                Amount::from_sat(500),
                FundingRate::new(dec!(0.001)).unwrap(),
            )),
            SignedAmount::from_sat(-500),
            dec!(-0.001).into(),
            "No price increase but fee means fee",
        );

        assert_profit_loss_values(
            Price::new(dec!(10_000)).unwrap(),
            Price::new(dec!(10_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::TWO,
            Leverage::ONE,
            empty_fee_short.add_funding_fee(FundingFee::new(
                Amount::from_sat(500),
                FundingRate::new(dec!(0.001)).unwrap(),
            )),
            SignedAmount::from_sat(500),
            dec!(0.0005).into(),
            "No price increase but fee means fee",
        );

        assert_profit_loss_values(
            Price::new(dec!(10_000)).unwrap(),
            Price::new(dec!(20_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::TWO,
            Leverage::ONE,
            empty_fee_long,
            SignedAmount::from_sat(50_000_000),
            dec!(100).into(),
            "A price increase of 2x should result in a profit of 100% (long)",
        );

        assert_profit_loss_values(
            Price::new(dec!(9_000)).unwrap(),
            Price::new(dec!(6_000)).unwrap(),
            Usd::new(dec!(9_000)),
            Leverage::TWO,
            Leverage::ONE,
            empty_fee_long,
            SignedAmount::from_sat(-50_000_000),
            dec!(-100).into(),
            "A price drop of 1/(Leverage + 1) x should result in 100% loss (long)",
        );

        assert_profit_loss_values(
            Price::new(dec!(10_000)).unwrap(),
            Price::new(dec!(5_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::TWO,
            Leverage::ONE,
            empty_fee_long,
            SignedAmount::from_sat(-50_000_000),
            dec!(-100).into(),
            "A loss should be capped at 100% (long)",
        );

        assert_profit_loss_values(
            Price::new(dec!(50_400)).unwrap(),
            Price::new(dec!(60_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::TWO,
            Leverage::ONE,
            empty_fee_long,
            SignedAmount::from_sat(3_174_603),
            dec!(31.999997984000016127999870976).into(),
            "long position should make a profit when price goes up",
        );

        assert_profit_loss_values(
            Price::new(dec!(50_400)).unwrap(),
            Price::new(dec!(60_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::TWO,
            Leverage::ONE,
            empty_fee_short,
            SignedAmount::from_sat(-3_174_603),
            dec!(-15.999998992000008063999935488).into(),
            "short position should make a loss when price goes up",
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn assert_profit_loss_values(
        initial_price: Price,
        closing_price: Price,
        quantity: Usd,
        leverage: Leverage,
        counterparty_leverage: Leverage,
        fee_account: FeeAccount,
        should_profit: SignedAmount,
        should_profit_in_percent: Percent,
        msg: &str,
    ) {
        // TODO: Assert on payout as well

        let (profit, in_percent, _) = calculate_profit_at_price(
            initial_price,
            closing_price,
            quantity,
            leverage,
            counterparty_leverage,
            fee_account,
        )
        .unwrap();

        assert_eq!(profit, should_profit, "{}", msg);
        assert_eq!(in_percent, should_profit_in_percent, "{}", msg);
    }

    #[test]
    fn test_profit_calculation_loss_plus_profit_should_be_zero() {
        let initial_price = Price::new(dec!(10_000)).unwrap();
        let closing_price = Price::new(dec!(16_000)).unwrap();
        let quantity = Usd::new(dec!(10_000));
        let leverage = Leverage::ONE;
        let counterparty_leverage = Leverage::ONE;

        let opening_fee = OpeningFee::new(Amount::from_sat(500));
        let funding_fee = FundingFee::new(
            Amount::from_sat(100),
            FundingRate::new(dec!(0.001)).unwrap(),
        );

        let taker_long = FeeAccount::new(Position::Long, Role::Taker)
            .add_opening_fee(opening_fee)
            .add_funding_fee(funding_fee);

        let maker_short = FeeAccount::new(Position::Short, Role::Maker)
            .add_opening_fee(opening_fee)
            .add_funding_fee(funding_fee);

        let (profit, profit_in_percent, _) = calculate_profit_at_price(
            initial_price,
            closing_price,
            quantity,
            leverage,
            counterparty_leverage,
            taker_long,
        )
        .unwrap();
        let (loss, loss_in_percent, _) = calculate_profit_at_price(
            initial_price,
            closing_price,
            quantity,
            leverage,
            counterparty_leverage,
            maker_short,
        )
        .unwrap();

        assert_eq!(profit.checked_add(loss).unwrap(), SignedAmount::ZERO);
        // NOTE:
        // this is only true when long_leverage == short_leverage
        assert_eq!(
            profit_in_percent.0.checked_add(loss_in_percent.0).unwrap(),
            Decimal::ZERO
        );
    }

    #[test]
    fn margin_remains_constant() {
        let initial_price = Price::new(dec!(15_000)).unwrap();
        let quantity = Usd::new(dec!(10_000));
        let leverage = Leverage::TWO;
        let counterpart_leverage = Leverage::ONE;

        let long_margin = calculate_margin(initial_price, quantity, leverage)
            .to_signed()
            .unwrap();
        let short_margin = calculate_margin(initial_price, quantity, Leverage::ONE)
            .to_signed()
            .unwrap();
        let pool_amount = SignedAmount::ONE_BTC;
        let closing_prices = [
            Price::new(dec!(0.15)).unwrap(),
            Price::new(dec!(1.5)).unwrap(),
            Price::new(dec!(15)).unwrap(),
            Price::new(dec!(150)).unwrap(),
            Price::new(dec!(1_500)).unwrap(),
            Price::new(dec!(15_000)).unwrap(),
            Price::new(dec!(150_000)).unwrap(),
            Price::new(dec!(1_500_000)).unwrap(),
            Price::new(dec!(15_000_000)).unwrap(),
        ];

        let opening_fee = OpeningFee::new(Amount::from_sat(500));
        let funding_fee = FundingFee::new(
            Amount::from_sat(100),
            FundingRate::new(dec!(0.001)).unwrap(),
        );

        let taker_long = FeeAccount::new(Position::Long, Role::Taker)
            .add_opening_fee(opening_fee)
            .add_funding_fee(funding_fee);

        let maker_short = FeeAccount::new(Position::Short, Role::Maker)
            .add_opening_fee(opening_fee)
            .add_funding_fee(funding_fee);

        for price in closing_prices {
            let (long_profit, _, _) = calculate_profit_at_price(
                initial_price,
                price,
                quantity,
                leverage,
                counterpart_leverage,
                taker_long,
            )
            .unwrap();
            let (short_profit, _, _) = calculate_profit_at_price(
                initial_price,
                price,
                quantity,
                leverage,
                counterpart_leverage,
                maker_short,
            )
            .unwrap();

            assert_eq!(
                long_profit + long_margin + short_profit + short_margin,
                pool_amount
            );
        }
    }

    #[test]
    fn order_id_serde_roundtrip() {
        let id = OrderId::default();

        let deserialized = serde_json::from_str(&serde_json::to_string(&id).unwrap()).unwrap();

        assert_eq!(id, deserialized);
    }

    #[test]
    fn cfd_event_to_json() {
        let event = EventKind::ContractSetupFailed;

        let (name, data) = event.to_json();

        assert_eq!(name, "ContractSetupFailed");
        assert_eq!(data, r#"null"#);
    }

    #[test]
    fn cfd_event_from_json() {
        let name = "ContractSetupFailed".to_owned();
        let data = r#"null"#.to_owned();

        let event = EventKind::from_json(name, data).unwrap();

        assert_eq!(event, EventKind::ContractSetupFailed);
    }

    #[test]
    fn cfd_ensure_stable_names_for_expensive_events() {
        let (rollover_event_name, _) = EventKind::RolloverCompleted {
            dlc: Dlc::dummy(None),
            funding_fee: FundingFee::new(Amount::ZERO, FundingRate::default()),
        }
        .to_json();

        let (setup_event_name, _) = EventKind::ContractSetupCompleted {
            dlc: Dlc::dummy(None),
        }
        .to_json();

        assert_eq!(
            setup_event_name,
            EventKind::CONTRACT_SETUP_COMPLETED_EVENT.to_owned()
        );
        assert_eq!(
            rollover_event_name,
            EventKind::ROLLOVER_COMPLETED_EVENT.to_owned()
        );
    }

    #[test]
    fn cfd_ensure_stable_names_for_load_filter_in_db() {
        let (collaborative_settlement_confirmed, _) =
            EventKind::CollaborativeSettlementConfirmed.to_json();
        let (cet_confirmed, _) = EventKind::CetConfirmed.to_json();
        let (refund_confirmed, _) = EventKind::RefundConfirmed.to_json();
        let (setup_failed, _) = EventKind::ContractSetupFailed.to_json();
        let (rejected, _) = EventKind::OfferRejected.to_json();

        assert_eq!(
            collaborative_settlement_confirmed,
            EventKind::COLLABORATIVE_SETTLEMENT_CONFIRMED.to_owned()
        );
        assert_eq!(cet_confirmed, EventKind::CET_CONFIRMED.to_owned());
        assert_eq!(refund_confirmed, EventKind::REFUND_CONFIRMED.to_owned());
        assert_eq!(setup_failed, EventKind::CONTRACT_SETUP_FAILED.to_owned());
        assert_eq!(rejected, EventKind::OFFER_REJECTED.to_owned());
    }

    #[test]
    fn cfd_event_no_data_from_json() {
        let name = "OfferRejected".to_owned();
        let data = r#"null"#.to_owned();

        let event = EventKind::from_json(name, data).unwrap();

        assert_eq!(event, EventKind::OfferRejected);
    }

    #[test]
    fn given_cfd_expires_now_then_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //                                                          now

        let cfd = Cfd::dummy_taker_long().dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));
        let result = cfd.can_auto_rollover_taker(datetime!(2021-11-19 10:00:00).assume_utc());

        assert!(result.is_ok());
    }

    #[test]
    fn given_cfd_expires_within_23hours_then_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //        now

        let cfd = Cfd::dummy_taker_long().dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));

        let result = cfd.can_auto_rollover_taker(datetime!(2021-11-18 11:00:00).assume_utc());

        assert!(result.is_ok());
    }

    #[test]
    fn given_cfd_was_just_rolled_over_then_no_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //    now

        let cfd = Cfd::dummy_taker_long().dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));
        let cannot_roll_over = cfd
            .can_auto_rollover_taker(datetime!(2021-11-18 10:00:01).assume_utc())
            .unwrap_err();

        assert_eq!(cannot_roll_over, NoRolloverReason::TooRecent)
    }

    #[test]
    fn given_cfd_out_of_bounds_expiry_then_no_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //  now

        let cfd = Cfd::dummy_taker_long().dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));
        let cannot_roll_over = cfd
            .can_auto_rollover_taker(datetime!(2021-11-18 09:59:59).assume_utc())
            .unwrap_err();

        assert_eq!(cannot_roll_over, NoRolloverReason::TooRecent)
    }

    #[test]
    fn given_cfd_was_renewed_less_than_1h_ago_then_no_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //       now

        let cfd = Cfd::dummy_taker_long().dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));
        let cannot_roll_over = cfd
            .can_auto_rollover_taker(datetime!(2021-11-18 10:59:59).assume_utc())
            .unwrap_err();

        assert_eq!(cannot_roll_over, NoRolloverReason::TooRecent)
    }

    #[test]
    fn given_cfd_not_locked_then_no_rollover() {
        let cfd = Cfd::dummy_not_open_yet();

        let cannot_roll_over = cfd.can_rollover().unwrap_err();

        assert!(matches!(
            cannot_roll_over,
            NoRolloverReason::NotLocked { .. }
        ))
    }

    #[test]
    fn given_cfd_has_attestation_then_no_rollover() {
        let cfd = Cfd::dummy_with_attestation(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));

        let cannot_roll_over = cfd.can_rollover().unwrap_err();

        assert!(matches!(cannot_roll_over, NoRolloverReason::Closed))
    }

    #[test]
    fn given_cfd_final_then_no_rollover() {
        let cfd = Cfd::dummy_final(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));

        let cannot_roll_over = cfd.can_rollover().unwrap_err();

        assert!(matches!(cannot_roll_over, NoRolloverReason::Closed))
    }

    #[test]
    fn can_calculate_funding_fee_with_negative_funding_rate() {
        let funding_rate = FundingRate::new(Decimal::NEGATIVE_ONE).unwrap();
        let funding_fee = FundingFee::calculate(
            Price::new(dec!(1)).unwrap(),
            Usd::new(dec!(1)),
            Leverage::ONE,
            Leverage::ONE,
            funding_rate,
            SETTLEMENT_INTERVAL.whole_hours(),
        )
        .unwrap();

        assert_eq!(funding_fee.fee, Amount::ONE_BTC);
        assert_eq!(funding_fee.rate, funding_rate);
    }

    #[test]
    fn given_collab_settlement_then_cannot_start_rollover() {
        let quantity = Usd::new(dec!(10));
        let opening_price = Price::new(dec!(10000)).unwrap();

        let taker_keys = keypair::new(&mut thread_rng());
        let maker_keys = keypair::new(&mut thread_rng());

        let (cfd, _, _, _) = Cfd::dummy_taker_long()
            .with_quantity(quantity)
            .with_opening_price(opening_price)
            .dummy_open(dummy_event_id())
            .with_lock(taker_keys, maker_keys)
            .dummy_collab_settlement_taker(opening_price);

        let result = cfd.start_rollover();

        let no_rollover_reason = result.unwrap_err().downcast::<NoRolloverReason>().unwrap();
        assert_eq!(no_rollover_reason, NoRolloverReason::Closed);
    }

    /// Cover scenario where trigger a collab settlement during ongoing rollover
    ///
    /// In this scenario the collab settlement finished before the rollover finished.
    /// If we trigger a collaborative settlement during rollover, the settlement will have priority
    /// over the rollover. Upon finishing the rollover we fail because the cfd was already
    /// settled.
    #[test]
    fn given_collab_settlement_finished_then_cannot_finish_rollover() {
        let quantity = Usd::new(dec!(10));
        let opening_price = Price::new(dec!(10000)).unwrap();

        let taker_keys = keypair::new(&mut thread_rng());
        let maker_keys = keypair::new(&mut thread_rng());

        let (cfd, _, _, _) = Cfd::dummy_taker_long()
            .with_quantity(quantity)
            .with_opening_price(opening_price)
            .dummy_open(dummy_event_id())
            .with_lock(taker_keys, maker_keys)
            .dummy_collab_settlement_taker(opening_price);

        let rollover_event = cfd.complete_rollover(Dlc::dummy(None), FundingFee::dummy());

        assert_eq!(rollover_event.event, EventKind::RolloverFailed);
    }

    /// Cover scenario where trigger a collab settlement during ongoing rollover
    ///
    /// In this scenario the collab settlement is still ongoing when the rollover finishes.
    /// If we trigger a collaborative settlement during rollover, the settlement will have priority
    /// over the rollover. Upon finishing the rollover we fail because the cfd was already
    /// settled.
    #[test]
    fn given_ongoing_collab_settlement_then_cannot_finish_rollover() {
        let cfd = Cfd::dummy_taker_long()
            .dummy_open(dummy_event_id())
            .dummy_start_collab_settlement();

        let rollover_event = cfd.complete_rollover(Dlc::dummy(None), FundingFee::dummy());

        assert_eq!(rollover_event.event, EventKind::RolloverFailed);
    }

    #[test]
    fn given_ongoing_collab_settlement_then_cannot_start_rollover() {
        let cfd = Cfd::dummy_taker_long()
            .dummy_open(dummy_event_id())
            .dummy_start_collab_settlement();

        let result = cfd.start_rollover();

        let no_rollover_reason = result.unwrap_err().downcast::<NoRolloverReason>().unwrap();
        assert_eq!(
            no_rollover_reason,
            NoRolloverReason::InCollaborativeSettlement
        );

        let cfd = Cfd::dummy_maker_short()
            .dummy_open(dummy_event_id())
            .dummy_start_collab_settlement();

        let result = cfd.start_rollover();

        let no_rollover_reason = result.unwrap_err().downcast::<NoRolloverReason>().unwrap();
        assert_eq!(
            no_rollover_reason,
            NoRolloverReason::InCollaborativeSettlement
        );
    }

    #[test]
    fn given_ongoing_rollover_then_can_start_collaborative_settlement() {
        let quantity = Usd::new(dec!(10));
        let opening_price = Price::new(dec!(10000)).unwrap();
        let order_id = OrderId::default();

        let taker_keys = keypair::new(&mut thread_rng());
        let maker_keys = keypair::new(&mut thread_rng());

        let taker_long = Cfd::dummy_taker_long()
            .with_id(order_id)
            .with_quantity(quantity)
            .with_opening_price(opening_price)
            .dummy_open(dummy_event_id())
            .dummy_start_rollover()
            .with_lock(taker_keys, maker_keys);

        let maker_short = Cfd::dummy_maker_short()
            .with_id(order_id)
            .with_quantity(quantity)
            .with_opening_price(opening_price)
            .dummy_open(dummy_event_id())
            .with_lock(taker_keys, maker_keys)
            .dummy_start_rollover();

        let (taker_long, proposal, taker_sig, _) =
            taker_long.dummy_collab_settlement_taker(opening_price);

        let (maker_short, _) = maker_short.dummy_collab_settlement_maker(proposal, taker_sig);

        assert!(
            taker_long.collaborative_settlement_spend_tx.is_some(),
            "No settlement tx even though the settlement passed"
        );
        assert!(
            maker_short.collaborative_settlement_spend_tx.is_some(),
            "No settlement tx even though the settlement passed"
        );
    }

    #[test]
    fn given_collab_settlement_then_cannot_force_close() {
        let quantity = Usd::new(dec!(10));
        let opening_price = Price::new(dec!(10000)).unwrap();

        let taker_keys = keypair::new(&mut thread_rng());
        let maker_keys = keypair::new(&mut thread_rng());

        let cfd = Cfd::dummy_taker_long()
            .with_quantity(quantity)
            .with_opening_price(opening_price)
            .dummy_open(dummy_event_id())
            .with_lock(taker_keys, maker_keys);

        let (cfd, _, _, _) = cfd.dummy_collab_settlement_taker(opening_price);

        // TODO: Assert on the error string
        assert!(
            cfd.manual_commit_to_blockchain().is_err(),
            "Manual commit to blockchain did not error"
        );
        assert!(
            cfd.decrypt_cet(&olivia::Attestation::dummy())
                .unwrap()
                .is_none(),
            "The decrypted CET is not expected to be Some"
        );
    }

    #[test]
    fn given_commit_when_lock_confirmed_then_lock_confirmed_after_finality() {
        let taker_long = Cfd::dummy_taker_long()
            .dummy_open(dummy_event_id())
            .dummy_commit();

        let maker_short = Cfd::dummy_maker_short()
            .dummy_open(dummy_event_id())
            .dummy_commit();

        let taker_event = taker_long.handle_lock_confirmed();
        let maker_event = maker_short.handle_lock_confirmed();

        assert_eq!(taker_event.event, EventKind::LockConfirmedAfterFinality);
        assert_eq!(maker_event.event, EventKind::LockConfirmedAfterFinality);
    }

    #[test]
    fn given_ongoing_collab_settlement_when_lock_confirmed_then_lock_confirmed() {
        let taker_long = Cfd::dummy_taker_long()
            .dummy_open(dummy_event_id())
            .dummy_start_collab_settlement();

        let maker_short = Cfd::dummy_maker_short()
            .dummy_open(dummy_event_id())
            .dummy_start_collab_settlement();

        let taker_event = taker_long.handle_lock_confirmed();
        let maker_event = maker_short.handle_lock_confirmed();

        assert_eq!(taker_event.event, EventKind::LockConfirmed);
        assert_eq!(maker_event.event, EventKind::LockConfirmed);
    }

    #[test]
    fn given_collab_settlement_finished_when_lock_confirmed_then_lock_confirmed_after_finality() {
        let quantity = Usd::new(dec!(10));
        let opening_price = Price::new(dec!(10000)).unwrap();
        let order_id = OrderId::default();

        let taker_keys = keypair::new(&mut thread_rng());
        let maker_keys = keypair::new(&mut thread_rng());

        let taker_long = Cfd::dummy_taker_long()
            .with_id(order_id)
            .with_quantity(quantity)
            .with_opening_price(opening_price)
            .dummy_open(dummy_event_id())
            .with_lock(taker_keys, maker_keys);

        let maker_short = Cfd::dummy_maker_short()
            .with_id(order_id)
            .with_quantity(quantity)
            .with_opening_price(opening_price)
            .dummy_open(dummy_event_id())
            .with_lock(taker_keys, maker_keys);

        let (taker_long, proposal, taker_sig, _) =
            taker_long.dummy_collab_settlement_taker(opening_price);
        let (maker_short, _) = maker_short.dummy_collab_settlement_maker(proposal, taker_sig);

        let taker_event = taker_long.handle_lock_confirmed();
        let maker_event = maker_short.handle_lock_confirmed();

        assert_eq!(taker_event.event, EventKind::LockConfirmedAfterFinality);
        assert_eq!(maker_event.event, EventKind::LockConfirmedAfterFinality);
    }

    #[test]
    fn given_commit_then_cannot_collab_close() {
        let taker_long = Cfd::dummy_taker_long()
            .dummy_open(dummy_event_id())
            .dummy_commit();

        let maker_short = Cfd::dummy_maker_short()
            .dummy_open(dummy_event_id())
            .dummy_commit();

        let result_taker = taker_long.propose_collaborative_settlement(Price::dummy(), N_PAYOUTS);
        let result_maker = maker_short
            .receive_collaborative_settlement_proposal(SettlementProposal::dummy(), N_PAYOUTS);

        assert!(result_taker.is_err(), "When having commit tx available we should not be able to trigger collaborative settlement");
        assert!(result_maker.is_err(), "When having commit tx available we should not be able to trigger collaborative settlement");
    }

    #[test]
    fn given_no_rollover_then_no_rollover_fee() {
        let quantity = Usd::new(dec!(10));
        let opening_price = Price::new(dec!(10000)).unwrap();
        let closing_price = Price::new(dec!(10000)).unwrap();
        let positive_funding_rate = dec!(0.0001);

        let (long_payout, short_payout) = collab_settlement_taker_long_maker_short(
            quantity,
            opening_price,
            closing_price,
            positive_funding_rate,
            0,
            0,
        );

        // Expected payout at closing-price-interval defined by payout curve
        let payout_interval_taker_amount = 49668;
        let payout_interval_maker_amount = 100332;
        // Expected initial funding fee based on the funding rate and short-margin (because the rate
        // is positive meaning long pays short)
        let initial_funding_fee = 10;

        assert_eq!(
            long_payout,
            payout_interval_taker_amount - initial_funding_fee - TX_FEE_COLLAB_SETTLEMENT
        );
        assert_eq!(
            short_payout,
            payout_interval_maker_amount + initial_funding_fee - TX_FEE_COLLAB_SETTLEMENT
        );
    }

    #[test]
    fn given_one_rollover_with_positive_rate_then_long_pays_rollover_fee() {
        let quantity = Usd::new(dec!(10));
        let opening_price = Price::new(dec!(10000)).unwrap();
        let closing_price = Price::new(dec!(10000)).unwrap();
        let positive_funding_rate = dec!(0.0001);

        let (long_payout, short_payout) = collab_settlement_taker_long_maker_short(
            quantity,
            opening_price,
            closing_price,
            positive_funding_rate,
            1000,
            1,
        );

        // Expected payout at closing-price-interval defined by payout curve
        let payout_interval_taker_amount = 49668;
        let payout_interval_maker_amount = 100332;
        // Expected initial funding fee based on the funding rate and short-margin (because the rate
        // is positive meaning long pays short)
        let initial_funding_fee = 10;

        assert_eq!(
            long_payout,
            payout_interval_taker_amount - initial_funding_fee - TX_FEE_COLLAB_SETTLEMENT - 1000
        );
        assert_eq!(
            short_payout,
            payout_interval_maker_amount + initial_funding_fee - TX_FEE_COLLAB_SETTLEMENT + 1000
        );
    }

    #[test]
    fn given_two_rollover_with_positive_rate_then_long_pays_two_rollover_fees() {
        let quantity = Usd::new(dec!(10));
        let opening_price = Price::new(dec!(10000)).unwrap();
        let closing_price = Price::new(dec!(10000)).unwrap();
        let positive_funding_rate = dec!(0.0001);

        let (long_payout, short_payout) = collab_settlement_taker_long_maker_short(
            quantity,
            opening_price,
            closing_price,
            positive_funding_rate,
            1000,
            2,
        );

        // Expected payout at closing-price-interval defined by payout curve
        let payout_interval_taker_amount = 49668;
        let payout_interval_maker_amount = 100332;
        // Expected initial funding fee based on the funding rate and short-margin (because the rate
        // is positive meaning long pays short)
        let initial_funding_fee = 10;

        assert_eq!(
            long_payout,
            payout_interval_taker_amount - initial_funding_fee - TX_FEE_COLLAB_SETTLEMENT - 2000
        );
        assert_eq!(
            short_payout,
            payout_interval_maker_amount + initial_funding_fee - TX_FEE_COLLAB_SETTLEMENT + 2000
        );
    }

    // TODO: Payout cove calculation uderflows because long is not capped at zero!
    // #[test]
    // fn given_more_rollover_then_long_margin_with_positive_rate_then_long_gets_liquidated() {
    //     let quantity = Usd::new(dec!(10));
    //     let opening_price = Price::new(dec!(10000)).unwrap();
    //     let closing_price = Price::new(dec!(10000)).unwrap();
    //     let positive_funding_rate = dec!(0.0001);
    //
    //     let (long_payout, short_payout) = collab_settlement_taker_long_maker_short(
    //         quantity,
    //         opening_price,
    //         closing_price,
    //         positive_funding_rate,
    //         1000,
    //         50,
    //     );
    //
    //     // Expected payout at closing-price-interval defined by payout curve
    //     let payout_interval_taker_amount = 49668;
    //     let payout_interval_maker_amount = 100332;
    //
    //     assert_eq!(
    //         long_payout,
    //         0
    //     );
    //     assert_eq!(
    //         short_payout,
    //         payout_interval_maker_amount + payout_interval_taker_amount -
    // TX_FEE_COLLAB_SETTLEMENT     );
    // }

    #[test]
    fn given_taker_long_maker_short_production_values_then_collab_settlement_is_as_expected() {
        // The values for this test are from production on 05.02.2022
        // For testing purpose different values can be plugged in to ensure sanity / debugging

        let quantity = Usd::new(dec!(100));
        let opening_price = Price::new(dec!(41015.60)).unwrap();
        let closing_price = Price::new(dec!(40600)).unwrap();
        let positive_funding_rate = dec!(0.0005);

        let (taker_payout, maker_payout) = collab_settlement_taker_long_maker_short(
            quantity,
            opening_price,
            closing_price,
            positive_funding_rate,
            0,
            0,
        );

        assert_eq!(taker_payout, 119239);
        assert_eq!(maker_payout, 246306);
    }

    #[test]
    fn test_calculate_long_liquidation_price() {
        let leverage = Leverage::new(2).unwrap();
        let price = Price::new(dec!(60_000)).unwrap();

        let is_liquidation_price = calculate_long_liquidation_price(leverage, price);

        let should_liquidation_price = Price::new(dec!(40_000)).unwrap();
        assert_eq!(is_liquidation_price, should_liquidation_price);
    }

    #[test]
    fn test_calculate_short_liquidation_price() {
        let leverage = Leverage::new(2).unwrap();
        let price = Price::new(dec!(60_000)).unwrap();

        let is_liquidation_price = calculate_short_liquidation_price(leverage, price);

        let should_liquidation_price = Price::new(dec!(120_000)).unwrap();
        assert_eq!(is_liquidation_price, should_liquidation_price);
    }

    #[test]
    fn test_calculate_infite_liquidation_price() {
        let leverage = Leverage::new(1).unwrap();
        let price = Price::new(dec!(60_000)).unwrap();

        let is_liquidation_price = calculate_short_liquidation_price(leverage, price);

        let should_liquidation_price = Price::INFINITE;
        assert_eq!(is_liquidation_price, should_liquidation_price);
    }

    #[test]
    fn correctly_calculate_hours_to_extend_in_rollover() {
        let settlement_interval = SETTLEMENT_INTERVAL.whole_hours();
        for hour in 0..settlement_interval {
            let event_id_in_x_hours =
                BitMexPriceEventId::with_20_digits(OffsetDateTime::now_utc() + hour.hours());

            let taker = Cfd::dummy_taker_long().dummy_open(event_id_in_x_hours);
            let maker = Cfd::dummy_maker_short().dummy_open(event_id_in_x_hours);

            assert_eq!(
                taker.hours_to_extend_in_rollover().unwrap(),
                (settlement_interval - hour) as u64
            );

            assert_eq!(
                maker.hours_to_extend_in_rollover().unwrap(),
                (settlement_interval - hour) as u64
            );
        }
    }

    #[test]
    fn rollover_extends_time_to_live_by_settlement_interval_if_cfd_can_be_settled() {
        let event_id_1_hour_ago =
            BitMexPriceEventId::with_20_digits(OffsetDateTime::now_utc() - 1.hours());

        let taker = Cfd::dummy_taker_long().dummy_open(event_id_1_hour_ago);
        let maker = Cfd::dummy_maker_short().dummy_open(event_id_1_hour_ago);

        assert_eq!(
            taker.hours_to_extend_in_rollover().unwrap(),
            SETTLEMENT_INTERVAL.whole_hours() as u64
        );

        assert_eq!(
            maker.hours_to_extend_in_rollover().unwrap(),
            SETTLEMENT_INTERVAL.whole_hours() as u64
        );
    }

    #[test]
    fn cannot_rollover_if_time_to_live_is_longer_than_settlement_interval() {
        let more_than_settlement_interval_hours = SETTLEMENT_INTERVAL.whole_hours() + 2;
        let event_id_way_in_the_future = BitMexPriceEventId::with_20_digits(
            OffsetDateTime::now_utc() + more_than_settlement_interval_hours.hours(),
        );

        let taker = Cfd::dummy_taker_long().dummy_open(event_id_way_in_the_future);
        let maker = Cfd::dummy_maker_short().dummy_open(event_id_way_in_the_future);

        taker.hours_to_extend_in_rollover().unwrap_err();
        maker.hours_to_extend_in_rollover().unwrap_err();
    }

    proptest! {
        #[test]
        fn rollover_extended_by_one_hour_if_time_to_live_is_within_one_hour_of_settlement_interval(minutes in 0i64..=60) {
            let close_to_settlement_interval = SETTLEMENT_INTERVAL + minutes.minutes();
            let event_id_within_the_hour = BitMexPriceEventId::with_20_digits(
                OffsetDateTime::now_utc() + close_to_settlement_interval,
            );

            let taker = Cfd::dummy_taker_long().dummy_open(event_id_within_the_hour);
            let maker = Cfd::dummy_maker_short().dummy_open(event_id_within_the_hour);

            prop_assert_eq!(taker.hours_to_extend_in_rollover().unwrap(), 1);
            prop_assert_eq!(maker.hours_to_extend_in_rollover().unwrap(), 1);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn collab_settlement_taker_long_maker_short(
        quantity: Usd,
        opening_price: Price,
        closing_price: Price,
        funding_rate: Decimal,
        funding_fee_sat_per_rollover: u64,
        nr_of_rollovers: u8,
    ) -> (u64, u64) {
        // we need to agree on same order id
        let order_id = OrderId::default();

        let taker_long = Cfd::taker_long_from_order(
            Order::dummy_short()
                .with_price(opening_price)
                .with_funding_rate(FundingRate::new(funding_rate).unwrap()),
            quantity,
        )
        .with_id(order_id);

        let maker_short = Cfd::maker_short_from_order(
            Order::dummy_short()
                .with_price(opening_price)
                .with_funding_rate(FundingRate::new(funding_rate).unwrap()),
            quantity,
        )
        .with_id(order_id);

        let taker_keys = keypair::new(&mut thread_rng());
        let maker_keys = keypair::new(&mut thread_rng());

        let (taker_long, proposal, taker_sig, taker_script) = taker_long
            .dummy_open(dummy_event_id())
            .dummy_rollovers(funding_fee_sat_per_rollover, funding_rate, nr_of_rollovers)
            .with_lock(taker_keys, maker_keys)
            .dummy_collab_settlement_taker(closing_price);

        let (maker_short, maker_script) = maker_short
            .dummy_open(dummy_event_id())
            .dummy_rollovers(funding_fee_sat_per_rollover, funding_rate, nr_of_rollovers)
            .with_lock(taker_keys, maker_keys)
            .dummy_collab_settlement_maker(proposal, taker_sig);

        let taker_payout = taker_long.collab_settlement_payout(taker_script);
        let maker_payout = maker_short.collab_settlement_payout(maker_script);

        (taker_payout.as_sat(), maker_payout.as_sat())
    }

    proptest! {
        #[test]
        fn rollover_funding_fee_collected_incrementally_should_not_be_smaller_than_collected_once_per_settlement_interval(quantity in 1u64..100_000u64) {
            let funding_rate = FundingRate::new(dec!(0.01)).unwrap();
            let price = Price::new(dec!(10_000)).unwrap();
            let quantity = Usd::new(Decimal::from(quantity));
            let leverage = Leverage::ONE;

            let funding_fee_for_whole_interval =
                FundingFee::calculate(
                    price,
                    quantity, leverage , leverage, funding_rate, SETTLEMENT_INTERVAL.whole_hours()).unwrap();
            let funding_fee_for_one_hour =
                FundingFee::calculate(price, quantity, leverage, leverage, funding_rate, 1).unwrap();
            let fee_account = FeeAccount::new(Position::Long, Role::Taker);

            let fee_account_whole_interval = fee_account.add_funding_fee(funding_fee_for_whole_interval);
            let fee_account_one_hour = fee_account.add_funding_fee(funding_fee_for_one_hour);

            let total_balance_when_collected_hourly = fee_account_one_hour.balance().checked_mul(SETTLEMENT_INTERVAL.whole_hours()).unwrap();
            let total_balance_when_collected_for_whole_interval = fee_account_whole_interval.balance();

            prop_assert!(
                total_balance_when_collected_hourly >= total_balance_when_collected_for_whole_interval,
                "when charged per hour we should not be at loss as compared to charging once per settlement interval"
            );

            prop_assert!(
            total_balance_when_collected_hourly - total_balance_when_collected_for_whole_interval < SignedAmount::from_sat(30), "we should not overcharge"
       );
    }
    }

    #[test]
    fn given_order_creation_timestamp_outdated_then_order_outdated() {
        let creation_timestamp = Timestamp::now();
        let order = Order::dummy_short().with_creation_timestamp(creation_timestamp);

        let now =
            OffsetDateTime::now_utc() + Duration::seconds(Order::OUTDATED_AFTER_MINS * 60 + 1);

        assert!(order.is_creation_timestamp_outdated(now))
    }

    #[test]
    fn given_order_creation_timestamp_not_outdated_then_order_not_outdated() {
        let creation_timestamp = Timestamp::now();
        let order = Order::dummy_short().with_creation_timestamp(creation_timestamp);

        let now =
            OffsetDateTime::now_utc() + Duration::seconds(Order::OUTDATED_AFTER_MINS * 60 - 1);

        assert!(!order.is_creation_timestamp_outdated(now))
    }

    #[test]
    fn given_oracle_event_id_is_24h_in_the_future_then_sane_to_take() {
        // --|---------|---------|----------------------------------|--> time
        //   -1h       |         +1h                                24h
        // --|---------|<--------|--------------------------------->|--
        //             now

        let order = Order::dummy_short().with_oracle_event_id(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));

        let sane =
            order.is_oracle_event_timestamp_sane(datetime!(2021-11-18 10:00:00).assume_utc());
        assert!(sane)
    }

    #[test]
    fn given_oracle_event_id_is_23h_in_the_future_then_sane_to_take() {
        // --|---------|---------|----------------------------------|--> time
        //   -1h       |         +1h                                24h
        // --|---------|<--------|--------------------------------->|--
        //                       now

        let order = Order::dummy_short().with_oracle_event_id(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));

        let sane =
            order.is_oracle_event_timestamp_sane(datetime!(2021-11-18 11:00:00).assume_utc());
        assert!(sane, "the oracle event id is outdated")
    }

    #[test]
    fn given_oracle_event_id_is_25h_in_the_future_then_sane_to_take() {
        // --|---------|---------|----------------------------------|--> time
        //   -1h       |         +1h                                24h
        // --|---------|<--------|--------------------------------->|--
        //   now

        let order = Order::dummy_short().with_oracle_event_id(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));

        let sane =
            order.is_oracle_event_timestamp_sane(datetime!(2021-11-18 09:00:00).assume_utc());
        assert!(sane, "the oracle event id is to far in the future")
    }

    #[test]
    fn given_oracle_event_id_is_more_than_25h_in_the_future_then_not_sane_to_take() {
        // --|---------|---------|----------------------------------|--> time
        //   -1h1s     |         +1h                                24h
        // --|---------|<--------|--------------------------------->|--
        //   now

        let order = Order::dummy_short().with_oracle_event_id(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));

        let sane =
            order.is_oracle_event_timestamp_sane(datetime!(2021-11-18 08:59:59).assume_utc());
        assert!(
            !sane,
            "an oracle event id that is too far in the future got accepted"
        )
    }

    #[test]
    fn given_oracle_event_id_is_less_than_23h_in_the_future_then_not_sane_to_take() {
        // --|---------|---------|----------------------------------|--> time
        //   -1h       |         +1h1s                              24h
        // --|---------|<--------|--------------------------------->|--
        //                       now

        let order = Order::dummy_short().with_oracle_event_id(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));

        let sane =
            order.is_oracle_event_timestamp_sane(datetime!(2021-11-18 11:00:01).assume_utc());
        assert!(!sane, "an oracle event id that is outdated got accepted")
    }

    impl CfdEvent {
        fn dummy_open(event_id: BitMexPriceEventId) -> Vec<Self> {
            vec![
                CfdEvent {
                    timestamp: Timestamp::now(),
                    id: Default::default(),
                    event: EventKind::ContractSetupStarted,
                },
                CfdEvent {
                    timestamp: Timestamp::now(),
                    id: Default::default(),
                    event: EventKind::ContractSetupCompleted {
                        dlc: Dlc::dummy(Some(event_id)),
                    },
                },
                CfdEvent {
                    timestamp: Timestamp::now(),
                    id: Default::default(),
                    event: EventKind::LockConfirmed,
                },
            ]
        }

        fn dummy_start_collab_settlement(order_id: OrderId) -> Vec<Self> {
            vec![CfdEvent {
                timestamp: Timestamp::now(),
                id: order_id,
                event: EventKind::CollaborativeSettlementStarted {
                    proposal: SettlementProposal {
                        order_id,
                        timestamp: Timestamp::now(),
                        taker: Default::default(),
                        maker: Default::default(),
                        price: Price::new(dec!(10000)).unwrap(),
                    },
                },
            }]
        }

        fn dummy_start_rollover() -> Vec<Self> {
            vec![CfdEvent {
                timestamp: Timestamp::now(),
                id: Default::default(),
                event: EventKind::RolloverStarted,
            }]
        }

        fn dummy_rollover(fee_sat: u64, funding_rate: Decimal) -> Vec<Self> {
            vec![
                CfdEvent {
                    timestamp: Timestamp::now(),
                    id: Default::default(),
                    event: EventKind::RolloverStarted,
                },
                CfdEvent {
                    timestamp: Timestamp::now(),
                    id: Default::default(),
                    event: EventKind::RolloverCompleted {
                        dlc: Dlc::dummy(Some(dummy_event_id())),
                        funding_fee: FundingFee {
                            fee: Amount::from_sat(fee_sat),
                            rate: FundingRate::new(funding_rate).unwrap(),
                        },
                    },
                },
            ]
        }

        fn dummy_attestation_prior_timelock(event_id: BitMexPriceEventId) -> Vec<Self> {
            let mut open = Self::dummy_open(event_id);
            open.push(CfdEvent {
                timestamp: Timestamp::now(),
                id: Default::default(),
                event: EventKind::OracleAttestedPriorCetTimelock {
                    timelocked_cet: dummy_transaction(),
                    commit_tx: Some(dummy_transaction()),
                    price: Price(dec!(10000)),
                },
            });

            open
        }

        fn dummy_manual_commit() -> Vec<Self> {
            vec![CfdEvent {
                timestamp: Timestamp::now(),
                id: Default::default(),
                event: EventKind::ManualCommit {
                    tx: dummy_transaction(),
                },
            }]
        }

        fn dummy_final_cet(event_id: BitMexPriceEventId) -> Vec<Self> {
            let mut open = Self::dummy_open(event_id);
            open.push(CfdEvent {
                timestamp: Timestamp::now(),
                id: Default::default(),
                event: EventKind::CetConfirmed,
            });

            open
        }
    }

    impl Cfd {
        fn taker_long_from_order(mut order: Order, quantity: Usd) -> Self {
            order.origin = Origin::Theirs;

            Cfd::from_order(order, quantity, dummy_identity(), Role::Taker)
        }

        fn maker_short_from_order(order: Order, quantity: Usd) -> Self {
            Cfd::from_order(order, quantity, dummy_identity(), Role::Maker)
        }

        fn dummy_taker_long() -> Self {
            Cfd::from_order(
                Order::dummy_short(),
                Usd::new(dec!(1000)),
                dummy_identity(),
                Role::Taker,
            )
        }

        fn dummy_maker_short() -> Self {
            Cfd::from_order(
                Order::dummy_short(),
                Usd::new(dec!(1000)),
                dummy_identity(),
                Role::Maker,
            )
        }

        fn dummy_not_open_yet() -> Self {
            Cfd::from_order(
                Order::dummy_short(),
                Usd::new(dec!(1000)),
                dummy_identity(),
                Role::Taker,
            )
        }

        fn dummy_open(self, event_id: BitMexPriceEventId) -> Self {
            CfdEvent::dummy_open(event_id)
                .into_iter()
                .fold(self, Cfd::apply)
        }

        /// Constructs a lock transaction from test wallet
        ///
        /// The transaction crated is not just a dummy, but is an actual lock transaction created
        /// according to the CFD values. This can be used to plug in a lock transaction into
        /// a dlc once we want to assert on spending from lock.
        fn with_lock(
            mut self,
            taker_keys: (SecretKey, PublicKey),
            maker_keys: (SecretKey, PublicKey),
        ) -> Self {
            let (sk_taker, pk_taker) = taker_keys;
            let (sk_maker, pk_maker) = maker_keys;

            match self.role {
                Role::Taker => {
                    let taker_margin = self.margin();
                    let maker_margin = self.counterparty_margin();

                    self.dlc = Some(self.dlc.unwrap().with_lock_taker(
                        taker_margin,
                        maker_margin,
                        sk_taker,
                        pk_maker,
                    ));
                }
                Role::Maker => {
                    let taker_margin = self.counterparty_margin();
                    let maker_margin = self.margin();

                    self.dlc = Some(self.dlc.unwrap().with_lock_maker(
                        taker_margin,
                        maker_margin,
                        sk_maker,
                        pk_taker,
                    ));
                }
            };

            self
        }

        fn dummy_start_rollover(self) -> Self {
            CfdEvent::dummy_start_rollover()
                .into_iter()
                .fold(self, Cfd::apply)
        }

        fn dummy_rollovers(self, fee_sat: u64, funding_rate: Decimal, nr_of_rollovers: u8) -> Self {
            let mut events = Vec::new();

            for _ in 0..nr_of_rollovers {
                let mut rollover = CfdEvent::dummy_rollover(fee_sat, funding_rate);
                events.append(&mut rollover)
            }

            events.into_iter().fold(self, Cfd::apply)
        }

        fn dummy_start_collab_settlement(self) -> Self {
            CfdEvent::dummy_start_collab_settlement(self.id)
                .into_iter()
                .fold(self, Cfd::apply)
        }

        fn dummy_collab_settlement_taker(
            self,
            price: Price,
        ) -> (Self, SettlementProposal, Signature, Script) {
            let mut events = Vec::new();

            let (propose, settlement_proposal) = self
                .propose_collaborative_settlement(price, N_PAYOUTS)
                .unwrap();
            events.push(propose);

            let (spend_tx, taker_signature, taker_script) = self
                .sign_collaborative_settlement_taker(&settlement_proposal)
                .unwrap();

            let settlement =
                CollaborativeSettlement::new(spend_tx, taker_script.clone(), price).unwrap();

            let settle = self.clone().complete_collaborative_settlement(settlement);
            events.push(settle);

            let cfd = events.into_iter().fold(self, Cfd::apply);

            (cfd, settlement_proposal, taker_signature, taker_script)
        }

        fn dummy_collab_settlement_maker(
            self,
            proposal: SettlementProposal,
            taker_signature: Signature,
        ) -> (Self, Script) {
            // handle receiving
            let mut events = Vec::new();
            let incoming_settlement = self
                .clone()
                .receive_collaborative_settlement_proposal(proposal, N_PAYOUTS)
                .unwrap();
            events.push(incoming_settlement);

            // apply receive because upon acceptance we ensure that we have a proposal (；￣Д￣）
            let cfd = events.into_iter().fold(self, Cfd::apply);

            let mut events = Vec::new();

            let accept = cfd
                .clone()
                .accept_collaborative_settlement_proposal(&proposal)
                .unwrap();
            events.push(accept);

            let settlement = cfd
                .sign_collaborative_settlement_maker(proposal, taker_signature)
                .unwrap();
            let script_pubkey = settlement.script_pubkey.clone();

            let settle = cfd.clone().complete_collaborative_settlement(settlement);
            events.push(settle);

            let cfd = events.into_iter().fold(cfd, Cfd::apply);

            (cfd, script_pubkey)
        }

        fn dummy_commit(self) -> Self {
            CfdEvent::dummy_manual_commit()
                .into_iter()
                .fold(self, Cfd::apply)
        }

        fn dummy_with_attestation(event_id: BitMexPriceEventId) -> Self {
            let cfd = Cfd::from_order(
                Order::dummy_short(),
                Usd::new(dec!(1000)),
                dummy_identity(),
                Role::Taker,
            );

            CfdEvent::dummy_attestation_prior_timelock(event_id)
                .into_iter()
                .fold(cfd, Cfd::apply)
        }

        fn dummy_final(event_id: BitMexPriceEventId) -> Self {
            let cfd = Cfd::from_order(
                Order::dummy_short(),
                Usd::new(dec!(1000)),
                dummy_identity(),
                Role::Taker,
            );

            CfdEvent::dummy_final_cet(event_id)
                .into_iter()
                .fold(cfd, Cfd::apply)
        }

        fn with_id(mut self, order_id: OrderId) -> Self {
            self.id = order_id;
            self
        }

        fn with_quantity(mut self, quantity: Usd) -> Self {
            self.quantity = quantity;
            self
        }

        fn with_opening_price(mut self, price: Price) -> Self {
            self.initial_price = price;
            self
        }

        fn collab_settlement_payout(self, script: Script) -> Amount {
            let tx = self.collaborative_settlement_spend_tx.unwrap();
            extract_payout_amount(tx, script)
        }
    }

    impl Order {
        fn dummy_short() -> Self {
            Order::new(
                Position::Short,
                Price::new(dec!(1000)).unwrap(),
                Usd::new(dec!(100)),
                Usd::new(dec!(1000)),
                Origin::Ours,
                dummy_event_id(),
                time::Duration::hours(24),
                TxFeeRate::default(),
                FundingRate::default(),
                OpeningFee::default(),
            )
        }

        fn with_price(mut self, price: Price) -> Self {
            self.price = price;
            self
        }

        fn with_funding_rate(mut self, funding_rate: FundingRate) -> Self {
            self.funding_rate = funding_rate;
            self
        }

        fn with_creation_timestamp(mut self, creation_timestamp: Timestamp) -> Self {
            self.creation_timestamp_maker = creation_timestamp;
            self
        }

        fn with_oracle_event_id(mut self, event_id: BitMexPriceEventId) -> Self {
            self.oracle_event_id = event_id;
            self
        }
    }

    impl Dlc {
        fn with_lock_maker(
            self,
            amount_taker: Amount,
            amount_maker: Amount,
            identity_sk: SecretKey,
            identity_counterparty_pk: PublicKey,
        ) -> Self {
            let maker_pk = bitcoin::PublicKey::new(identity_sk.to_public_key());
            let taker_pk = identity_counterparty_pk;
            let descriptor = lock_descriptor(maker_pk, taker_pk);

            self.with_lock(
                amount_taker,
                amount_maker,
                identity_sk,
                identity_counterparty_pk,
                descriptor,
            )
        }

        fn with_lock_taker(
            self,
            amount_taker: Amount,
            amount_maker: Amount,
            identity_sk: SecretKey,
            identity_counterparty_pk: PublicKey,
        ) -> Self {
            let maker_pk = identity_counterparty_pk;
            let taker_pk = bitcoin::PublicKey::new(identity_sk.to_public_key());
            let descriptor = lock_descriptor(maker_pk, taker_pk);

            self.with_lock(
                amount_taker,
                amount_maker,
                identity_sk,
                identity_counterparty_pk,
                descriptor,
            )
        }

        fn with_lock(
            mut self,
            amount_taker: Amount,
            amount_maker: Amount,
            identity_sk: SecretKey,
            identity_counterparty_pk: PublicKey,
            descriptor: Descriptor<PublicKey>,
        ) -> Self {
            let lock_tx = Transaction {
                version: 0,
                lock_time: 0,
                input: vec![],
                output: vec![TxOut {
                    value: amount_taker.as_sat() + amount_maker.as_sat(),
                    script_pubkey: descriptor.script_pubkey(),
                }],
            };

            self.lock = (lock_tx, descriptor);
            self.taker_lock_amount = amount_taker;
            self.maker_lock_amount = amount_maker;
            self.identity = identity_sk;
            self.identity_counterparty = identity_counterparty_pk;

            self.taker_address = Address::from_str("mz3SbgvUZGHaxDdRu7FtZ8MuoLgDPhLVta").unwrap();
            self.maker_address = Address::from_str("mzMcNcKMXQdwMpdgknDQnHZiMxnQKWZ4vh").unwrap();

            self
        }

        fn dummy(event_id: Option<BitMexPriceEventId>) -> Self {
            let dummy_sk = SecretKey::from_slice(&[1; 32]).unwrap();
            let dummy_pk = PublicKey::from_slice(&[
                3, 23, 183, 225, 206, 31, 159, 148, 195, 42, 67, 115, 146, 41, 248, 140, 11, 3, 51,
                41, 111, 180, 110, 143, 114, 134, 88, 73, 198, 174, 52, 184, 78,
            ])
            .unwrap();

            let dummy_addr = Address::from_str("132F25rTsvBdp9JzLLBHP5mvGY66i1xdiM").unwrap();

            let dummy_tx = dummy_partially_signed_transaction().extract_tx();
            let dummy_adapter_sig = "03424d14a5471c048ab87b3b83f6085d125d5864249ae4297a57c84e74710bb6730223f325042fce535d040fee52ec13231bf709ccd84233c6944b90317e62528b2527dff9d659a96db4c99f9750168308633c1867b70f3a18fb0f4539a1aecedcd1fc0148fc22f36b6303083ece3f872b18e35d368b3958efe5fb081f7716736ccb598d269aa3084d57e1855e1ea9a45efc10463bbf32ae378029f5763ceb40173f"
                .parse()
                .unwrap();

            let dummy_sig = Signature::from_str("3046022100839c1fbc5304de944f697c9f4b1d01d1faeba32d751c0f7acb21ac8a0f436a72022100e89bd46bb3a5a62adc679f659b7ce876d83ee297c7a5587b2011c4fcc72eab45").unwrap();

            let mut dummy_cet_with_zero_price_range = HashMap::new();
            dummy_cet_with_zero_price_range.insert(
                BitMexPriceEventId::with_20_digits(OffsetDateTime::now_utc()),
                vec![Cet {
                    maker_amount: Amount::from_sat(0),
                    taker_amount: Amount::from_sat(0),
                    adaptor_sig: dummy_adapter_sig,
                    range: RangeInclusive::new(0, 1),
                    n_bits: 0,
                    txid: dummy_tx.txid(),
                }],
            );

            Dlc {
                identity: dummy_sk,
                identity_counterparty: dummy_pk,
                revocation: dummy_sk,
                revocation_pk_counterparty: dummy_pk,
                publish: dummy_sk,
                publish_pk_counterparty: dummy_pk,
                maker_address: dummy_addr.clone(),
                taker_address: dummy_addr,
                lock: (dummy_tx.clone(), Descriptor::new_pk(dummy_pk)),
                commit: (
                    dummy_tx.clone(),
                    dummy_adapter_sig,
                    Descriptor::new_pk(dummy_pk),
                ),
                cets: dummy_cet_with_zero_price_range,
                refund: (dummy_tx, dummy_sig),
                maker_lock_amount: Default::default(),
                taker_lock_amount: Default::default(),
                revoked_commit: vec![],
                settlement_event_id: match event_id {
                    Some(event_id) => event_id,
                    None => dummy_event_id(),
                },
                refund_timelock: 0,
            }
        }
    }

    pub fn dummy_transaction() -> Transaction {
        dummy_partially_signed_transaction().extract_tx()
    }

    pub fn dummy_partially_signed_transaction() -> PartiallySignedTransaction {
        // very simple dummy psbt that does not contain anything
        // pulled in from github.com-1ecc6299db9ec823/bitcoin-0.27.1/src/util/psbt/mod.rs:238

        PartiallySignedTransaction {
            global: Global {
                unsigned_tx: Transaction {
                    version: 2,
                    lock_time: 0,
                    input: vec![],
                    output: vec![],
                },
                xpub: Default::default(),
                version: 0,
                proprietary: BTreeMap::new(),
                unknown: BTreeMap::new(),
            },
            inputs: vec![],
            outputs: vec![],
        }
    }

    pub fn dummy_identity() -> Identity {
        Identity::new(x25519_dalek::PublicKey::from(
            *b"hello world, oh what a beautiful",
        ))
    }

    pub fn dummy_event_id() -> BitMexPriceEventId {
        BitMexPriceEventId::with_20_digits(OffsetDateTime::now_utc())
    }

    fn extract_payout_amount(tx: Transaction, script: Script) -> Amount {
        tx.output
            .into_iter()
            .find(|tx_out| tx_out.script_pubkey == script)
            .map(|tx_out| Amount::from_sat(tx_out.value))
            .unwrap_or(Amount::ZERO)
    }

    impl olivia::Attestation {
        fn dummy() -> Self {
            Self {
                id: dummy_event_id(),
                price: 0,
                scalars: vec![],
            }
        }
    }

    impl FundingFee {
        fn dummy() -> Self {
            FundingFee::new(
                Amount::default(),
                FundingRate::new(Decimal::default()).unwrap(),
            )
        }
    }

    impl SettlementProposal {
        fn dummy() -> Self {
            SettlementProposal {
                order_id: Default::default(),
                timestamp: Timestamp::now(),
                taker: Default::default(),
                maker: Default::default(),
                price: Price::new(Decimal::ONE).unwrap(),
            }
        }
    }

    impl Price {
        fn dummy() -> Self {
            Price::new(Decimal::ONE).unwrap()
        }
    }

    /// The transaction fee for collaborative settlement in sats for each party
    ///
    /// This is based on the fact that collaborative settlement uses a fixed fee rate of 1
    /// sat/vbytes. This constant represents what each party pays, i.e. the split fee for each
    /// party.
    const TX_FEE_COLLAB_SETTLEMENT: u64 = 85;

    const N_PAYOUTS: usize = 200;
}
