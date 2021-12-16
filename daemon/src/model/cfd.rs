use crate::model::BitMexPriceEventId;
use crate::model::Identity;
use crate::model::InversePrice;
use crate::model::Leverage;
use crate::model::Percent;
use crate::model::Position;
use crate::model::Price;
use crate::model::Timestamp;
use crate::model::TradingPair;
use crate::model::Usd;
use crate::monitor;
use crate::oracle;
use crate::payout_curve;
use crate::SETTLEMENT_INTERVAL;
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
use bdk::bitcoin::Txid;
use bdk::descriptor::Descriptor;
use bdk::miniscript::DescriptorTrait;
use maia::finalize_spend_transaction;
use maia::secp256k1_zkp;
use maia::secp256k1_zkp::EcdsaAdaptorSignature;
use maia::secp256k1_zkp::SECP256K1;
use maia::spending_tx_sighash;
use maia::TransactionExt;
use rocket::request::FromParam;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::de::Error as _;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::ops::RangeInclusive;
use time::Duration;
use time::OffsetDateTime;
use uuid::adapter::Hyphenated;
use uuid::Uuid;

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

impl<'v> FromParam<'v> for OrderId {
    type Error = uuid::Error;

    fn from_param(param: &'v str) -> Result<Self, Self::Error> {
        let uuid = param.parse::<Uuid>()?;
        Ok(OrderId(uuid.to_hyphenated()))
    }
}

// TODO: Could potentially remove this and use the Role in the Order instead
/// Origin of the order
#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, sqlx::Type)]
pub enum Origin {
    Ours,
    Theirs,
}

/// Role in the Cfd
#[derive(Debug, Copy, Clone, PartialEq, sqlx::Type)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Order {
    pub id: OrderId,

    pub trading_pair: TradingPair,
    pub position: Position,

    pub price: Price,

    // TODO: [post-MVP] Representation of the contract size; at the moment the contract size is
    //  always 1 USD
    pub min_quantity: Usd,
    pub max_quantity: Usd,

    pub leverage: Leverage,
    pub liquidation_price: Price,

    pub creation_timestamp: Timestamp,

    /// The duration that will be used for calculating the settlement timestamp
    pub settlement_interval: Duration,

    pub origin: Origin,

    /// The id of the event to be used for price attestation
    ///
    /// The maker includes this into the Order based on the Oracle announcement to be used.
    pub oracle_event_id: BitMexPriceEventId,

    pub fee_rate: u32,
}

impl Order {
    pub fn new_short(
        price: Price,
        min_quantity: Usd,
        max_quantity: Usd,
        origin: Origin,
        oracle_event_id: BitMexPriceEventId,
        settlement_interval: Duration,
        fee_rate: u32,
    ) -> Result<Self> {
        let leverage = Leverage::new(2)?;
        let liquidation_price = calculate_long_liquidation_price(leverage, price);

        Ok(Order {
            id: OrderId::default(),
            price,
            min_quantity,
            max_quantity,
            leverage,
            trading_pair: TradingPair::BtcUsd,
            liquidation_price,
            position: Position::Short,
            creation_timestamp: Timestamp::now(),
            settlement_interval,
            origin,
            oracle_event_id,
            fee_rate,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Error {
    Connect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CfdStateError {
    last_successful_state: CfdState,
    error: Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct CfdStateCommon {
    pub transition_timestamp: Timestamp,
}

impl Default for CfdStateCommon {
    fn default() -> Self {
        Self {
            transition_timestamp: Timestamp::now(),
        }
    }
}

// Note: De-/Serialize with type tag to make handling on UI easier
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum CfdState {
    /// The taker sent an order to the maker to open the CFD but doesn't have a response yet.
    ///
    /// This state applies to taker only.
    OutgoingOrderRequest { common: CfdStateCommon },

    /// The maker received an order from the taker to open the CFD but doesn't have a response yet.
    ///
    /// This state applies to the maker only.
    IncomingOrderRequest {
        common: CfdStateCommon,
        taker_id: Identity,
    },

    /// The maker has accepted the CFD take request, but the contract is not set up on chain yet.
    ///
    /// This state applies to taker and maker.
    Accepted { common: CfdStateCommon },

    /// The maker rejected the CFD order.
    ///
    /// This state applies to taker and maker.
    /// This is a final state.
    Rejected { common: CfdStateCommon },

    /// State used during contract setup.
    ///
    /// This state applies to taker and maker.
    /// All contract setup messages between taker and maker are expected to be sent in on scope.
    ContractSetup { common: CfdStateCommon },

    PendingOpen {
        common: CfdStateCommon,
        dlc: Dlc,
        attestation: Option<Attestation>,
    },

    /// The CFD contract is set up on chain.
    ///
    /// This state applies to taker and maker.
    Open {
        common: CfdStateCommon,
        dlc: Dlc,
        attestation: Option<Attestation>,
        collaborative_close: Option<CollaborativeSettlement>,
    },

    /// The commit transaction was published but it not final yet
    ///
    /// This state applies to taker and maker.
    /// This state is needed, because otherwise the user does not get any feedback.
    PendingCommit {
        common: CfdStateCommon,
        dlc: Dlc,
        attestation: Option<Attestation>,
    },

    // TODO: At the moment we are appending to this state. The way this is handled internally is
    //  by inserting the same state with more information in the database. We could consider
    //  changing this to insert different states or update the stae instead of inserting again.
    /// The CFD contract's commit transaction reached finality on chain
    ///
    /// This means that the commit transaction was detected on chain and reached finality
    /// confirmations and the contract will be forced to close.
    OpenCommitted {
        common: CfdStateCommon,
        dlc: Dlc,
        cet_status: CetStatus,
    },

    /// The CET was published on chain but is not final yet
    ///
    /// This state applies to taker and maker.
    /// This state is needed, because otherwise the user does not get any feedback.
    PendingCet {
        common: CfdStateCommon,
        dlc: Dlc,
        attestation: Attestation,
    },

    /// The position was closed collaboratively or non-collaboratively
    ///
    /// This state applies to taker and maker.
    /// This is a final state.
    /// This is the final state for all happy-path scenarios where we had an open position and then
    /// "settled" it. Settlement can be collaboratively or non-collaboratively (by publishing
    /// commit + cet).
    Closed {
        common: CfdStateCommon,
        payout: Payout,
    },

    // TODO: Can be extended with CetStatus
    /// The CFD contract's refund transaction was published but it not final yet
    PendingRefund { common: CfdStateCommon, dlc: Dlc },

    /// The Cfd was refunded and the refund transaction reached finality
    ///
    /// This state applies to taker and maker.
    /// This is a final state.
    Refunded { common: CfdStateCommon, dlc: Dlc },

    /// The Cfd was in a state that could not be continued after the application got interrupted
    ///
    /// This state applies to taker and maker.
    /// This is a final state.
    /// It is safe to remove Cfds in this state from the database.
    SetupFailed {
        common: CfdStateCommon,
        info: String,
    },
}

impl CfdState {
    pub fn outgoing_order_request() -> Self {
        Self::OutgoingOrderRequest {
            common: CfdStateCommon::default(),
        }
    }

    pub fn accepted() -> Self {
        Self::Accepted {
            common: CfdStateCommon::default(),
        }
    }

    pub fn rejected() -> Self {
        Self::Rejected {
            common: CfdStateCommon::default(),
        }
    }

    pub fn contract_setup() -> Self {
        Self::ContractSetup {
            common: CfdStateCommon::default(),
        }
    }

    pub fn closed(payout: Payout) -> Self {
        Self::Closed {
            common: CfdStateCommon::default(),
            payout,
        }
    }

    pub fn must_refund(dlc: Dlc) -> Self {
        Self::PendingRefund {
            common: CfdStateCommon::default(),
            dlc,
        }
    }

    pub fn refunded(dlc: Dlc) -> Self {
        Self::Refunded {
            common: CfdStateCommon::default(),
            dlc,
        }
    }

    pub fn setup_failed(info: String) -> Self {
        Self::SetupFailed {
            common: CfdStateCommon::default(),
            info,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Payout {
    CollaborativeClose(CollaborativeSettlement),
    Cet(Attestation),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attestation {
    pub id: BitMexPriceEventId,
    pub scalars: Vec<SecretKey>,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_sat")]
    payout: Amount,
    price: u64,
    txid: Txid,
}

impl Attestation {
    pub fn new(
        id: BitMexPriceEventId,
        price: u64,
        scalars: Vec<SecretKey>,
        dlc: Dlc,
        role: Role,
    ) -> Result<Self> {
        let cet = dlc
            .cets
            .iter()
            .find_map(|(_, cet)| cet.iter().find(|cet| cet.range.contains(&price)))
            .context("Unable to find attested price in any range")?;

        let txid = cet.tx.txid();

        let our_script_pubkey = dlc.script_pubkey_for(role);
        let payout = cet
            .tx
            .output
            .iter()
            .find_map(|output| {
                (output.script_pubkey == our_script_pubkey).then(|| Amount::from_sat(output.value))
            })
            .unwrap_or_default();

        Ok(Self {
            id,
            price,
            scalars,
            payout,
            txid,
        })
    }

    pub fn price(&self) -> Result<Price> {
        let dec = Decimal::from_u64(self.price).context("Could not convert u64 to decimal")?;
        Ok(Price::new(dec)?)
    }

    pub fn txid(&self) -> Txid {
        self.txid
    }

    pub fn payout(&self) -> Amount {
        self.payout
    }
}

impl From<Attestation> for oracle::Attestation {
    fn from(attestation: Attestation) -> oracle::Attestation {
        oracle::Attestation {
            id: attestation.id,
            price: attestation.price,
            scalars: attestation.scalars,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum CetStatus {
    Unprepared,
    TimelockExpired,
    OracleSigned(Attestation),
    Ready(Attestation),
}

impl fmt::Display for CetStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CetStatus::Unprepared => write!(f, "Unprepared"),
            CetStatus::TimelockExpired => write!(f, "TimelockExpired"),
            CetStatus::OracleSigned(_) => write!(f, "OracleSigned"),
            CetStatus::Ready(_) => write!(f, "Ready"),
        }
    }
}

impl CfdState {
    fn get_common(&self) -> CfdStateCommon {
        let common = match self {
            CfdState::OutgoingOrderRequest { common } => common,
            CfdState::IncomingOrderRequest { common, .. } => common,
            CfdState::Accepted { common } => common,
            CfdState::Rejected { common } => common,
            CfdState::ContractSetup { common } => common,
            CfdState::PendingOpen { common, .. } => common,
            CfdState::Open { common, .. } => common,
            CfdState::OpenCommitted { common, .. } => common,
            CfdState::PendingRefund { common, .. } => common,
            CfdState::Refunded { common, .. } => common,
            CfdState::SetupFailed { common, .. } => common,
            CfdState::PendingCommit { common, .. } => common,
            CfdState::PendingCet { common, .. } => common,
            CfdState::Closed { common, .. } => common,
        };

        *common
    }

    pub fn get_transition_timestamp(&self) -> Timestamp {
        self.get_common().transition_timestamp
    }

    pub fn get_collaborative_close(&self) -> Option<CollaborativeSettlement> {
        match self {
            CfdState::Open {
                collaborative_close,
                ..
            } => collaborative_close.clone(),
            _ => None,
        }
    }
}

impl fmt::Display for CfdState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CfdState::OutgoingOrderRequest { .. } => {
                write!(f, "Request sent")
            }
            CfdState::IncomingOrderRequest { .. } => {
                write!(f, "Requested")
            }
            CfdState::Accepted { .. } => {
                write!(f, "Accepted")
            }
            CfdState::Rejected { .. } => {
                write!(f, "Rejected")
            }
            CfdState::ContractSetup { .. } => {
                write!(f, "Contract Setup")
            }
            CfdState::PendingOpen { .. } => {
                write!(f, "Pending Open")
            }
            CfdState::Open { .. } => {
                write!(f, "Open")
            }
            CfdState::PendingCommit { .. } => {
                write!(f, "Pending Commit")
            }
            CfdState::OpenCommitted { .. } => {
                write!(f, "Open Committed")
            }
            CfdState::PendingRefund { .. } => {
                write!(f, "Must Refund")
            }
            CfdState::Refunded { .. } => {
                write!(f, "Refunded")
            }
            CfdState::SetupFailed { .. } => {
                write!(f, "Setup Failed")
            }
            CfdState::PendingCet { .. } => {
                write!(f, "Pending CET")
            }
            CfdState::Closed { .. } => {
                write!(f, "Closed")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum UpdateCfdProposal {
    Settlement {
        proposal: SettlementProposal,
        direction: SettlementKind,
    },
    RollOverProposal {
        proposal: RolloverProposal,
        direction: SettlementKind,
    },
}

/// Proposed collaborative settlement
#[derive(Debug, Clone)]
pub struct SettlementProposal {
    pub order_id: OrderId,
    pub timestamp: Timestamp,
    pub taker: Amount,
    pub maker: Amount,
    pub price: Price,
}

/// Proposed collaborative settlement
#[derive(Debug, Clone)]
pub struct RolloverProposal {
    pub order_id: OrderId,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone)]
pub enum SettlementKind {
    Incoming,
    Outgoing,
}

pub type UpdateCfdProposals = HashMap<OrderId, UpdateCfdProposal>;

/// Represents a cfd (including state)
#[derive(Debug, Clone, PartialEq)]
pub struct Cfd {
    id: OrderId,

    trading_pair: TradingPair,
    position: Position,

    price: Price,

    leverage: Leverage,
    liquidation_price: Price,

    creation_timestamp: Timestamp,

    /// The duration that will be used for calculating the settlement timestamp
    settlement_interval: Duration,

    role: Role,

    fee_rate: u32,

    quantity_usd: Usd,
    state: CfdState,

    counterparty: Identity,
}

impl Cfd {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: OrderId,
        trading_pair: TradingPair,
        position: Position,
        price: Price,
        leverage: Leverage,
        liquidation_price: Price,
        creation_timestamp: Timestamp,
        settlement_interval: Duration,
        role: Role,
        fee_rate: u32,
        quantity_usd: Usd,
        state: CfdState,
        counterparty: Identity,
    ) -> Self {
        Self {
            id,
            trading_pair,
            position,
            price,
            leverage,
            liquidation_price,
            creation_timestamp,
            settlement_interval,
            fee_rate,
            quantity_usd,
            state,
            counterparty,
            role,
        }
    }
}

impl Cfd {
    pub fn from_order(
        order: Order,
        quantity: Usd,
        state: CfdState,
        counterparty: Identity,
        role: Role,
    ) -> Self {
        Cfd {
            id: order.id,
            quantity_usd: quantity,
            state,
            trading_pair: order.trading_pair,
            position: match order.origin {
                Origin::Ours => order.position,
                Origin::Theirs => order.position.counter_position(),
            },
            price: order.price,
            leverage: order.leverage,
            liquidation_price: order.liquidation_price,
            creation_timestamp: Timestamp::now(),
            settlement_interval: order.settlement_interval,
            fee_rate: order.fee_rate,
            counterparty,
            role,
        }
    }

    pub fn margin(&self) -> Result<Amount> {
        let margin = match self.position() {
            Position::Long => calculate_long_margin(self.price, self.quantity_usd, self.leverage),
            Position::Short => calculate_short_margin(self.price, self.quantity_usd),
        };

        Ok(margin)
    }

    pub fn counterparty_margin(&self) -> Result<Amount> {
        let margin = match self.position() {
            Position::Long => calculate_short_margin(self.price, self.quantity_usd),
            Position::Short => calculate_long_margin(self.price, self.quantity_usd, self.leverage),
        };

        Ok(margin)
    }

    pub fn profit(&self, current_price: Price) -> Result<(SignedAmount, Percent)> {
        let closing_price = match (self.attestation(), self.collaborative_close()) {
            (Some(_attestation), Some(collaborative_close)) => collaborative_close.price,
            (None, Some(collaborative_close)) => collaborative_close.price,
            (Some(attestation), None) => attestation.price()?,
            (None, None) => current_price,
        };

        let (p_n_l, p_n_l_percent) = calculate_profit(
            self.price,
            closing_price,
            self.quantity_usd,
            self.leverage,
            self.position(),
        )?;

        Ok((p_n_l, p_n_l_percent))
    }

    pub fn calculate_settlement(
        &self,
        current_price: Price,
        n_payouts: usize,
    ) -> Result<SettlementProposal> {
        let payout_curve =
            payout_curve::calculate(self.price, self.quantity_usd, self.leverage, n_payouts)?;

        let payout = {
            let current_price = current_price.try_into_u64()?;
            payout_curve
                .iter()
                .find(|&x| x.digits().range().contains(&current_price))
                .context("find current price on the payout curve")?
        };

        let settlement = SettlementProposal {
            order_id: self.id,
            timestamp: Timestamp::now(),
            taker: *payout.taker_amount(),
            maker: *payout.maker_amount(),
            price: current_price,
        };

        Ok(settlement)
    }

    pub fn position(&self) -> Position {
        self.position
    }

    pub fn refund_timelock_in_blocks(&self) -> u32 {
        (self.settlement_interval * Cfd::REFUND_THRESHOLD)
            .as_blocks()
            .ceil() as u32
    }

    pub fn expiry_timestamp(&self) -> Option<OffsetDateTime> {
        self.dlc().map(|dlc| dlc.settlement_event_id.timestamp)
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

    pub const CET_TIMELOCK: u32 = 12;

    pub fn handle_monitoring_event(&mut self, event: monitor::Event) -> Result<Option<CfdState>> {
        use CfdState::*;

        let order_id = self.id;

        // early exit if already final
        if let SetupFailed { .. } | Closed { .. } | Refunded { .. } = self.state.clone() {
            tracing::trace!(
                "Ignoring monitoring event {:?} because cfd already in state {}",
                event,
                self.state
            );
            return Ok(None);
        }

        let new_state = match event {
            monitor::Event::LockFinality(_) => {
                if let PendingOpen { dlc, .. } = self.state.clone() {
                    CfdState::Open {
                        common: CfdStateCommon {
                            transition_timestamp: Timestamp::now(),
                        },
                        dlc,
                        attestation: None,
                        collaborative_close: None,
                    }
                } else if let Open {
                    dlc,
                    attestation,
                    collaborative_close,
                    ..
                } = self.state.clone()
                {
                    CfdState::Open {
                        common: CfdStateCommon {
                            transition_timestamp: Timestamp::now(),
                        },
                        dlc,
                        attestation,
                        collaborative_close,
                    }
                } else {
                    bail!(
                        "Cannot transition to Open because of unexpected state {}",
                        self.state
                    )
                }
            }
            monitor::Event::CommitFinality(_) => {
                let (dlc, attestation) = if let PendingCommit {
                    dlc, attestation, ..
                } = self.state.clone()
                {
                    (dlc, attestation)
                } else if let PendingOpen {
                    dlc, attestation, ..
                }
                | Open {
                    dlc, attestation, ..
                } = self.state.clone()
                {
                    tracing::debug!(%order_id, "Was in unexpected state {}, jumping ahead to OpenCommitted", self.state);
                    (dlc, attestation)
                } else {
                    bail!(
                        "Cannot transition to OpenCommitted because of unexpected state {}",
                        self.state
                    )
                };

                OpenCommitted {
                    common: CfdStateCommon {
                        transition_timestamp: Timestamp::now(),
                    },
                    dlc,
                    cet_status: if let Some(attestation) = attestation {
                        CetStatus::OracleSigned(attestation)
                    } else {
                        CetStatus::Unprepared
                    },
                }
            }
            monitor::Event::CloseFinality(_) => {
                let collaborative_close = self.collaborative_close().context(
                    "No collaborative close after reaching collaborative close finality",
                )?;

                CfdState::closed(Payout::CollaborativeClose(collaborative_close))
            }
            monitor::Event::CetTimelockExpired(_) => match self.state.clone() {
                CfdState::OpenCommitted {
                    dlc,
                    cet_status: CetStatus::Unprepared,
                    ..
                } => CfdState::OpenCommitted {
                    common: CfdStateCommon {
                        transition_timestamp: Timestamp::now(),
                    },
                    dlc,
                    cet_status: CetStatus::TimelockExpired,
                },
                CfdState::OpenCommitted {
                    dlc,
                    cet_status: CetStatus::OracleSigned(attestation),
                    ..
                } => CfdState::OpenCommitted {
                    common: CfdStateCommon {
                        transition_timestamp: Timestamp::now(),
                    },
                    dlc,
                    cet_status: CetStatus::Ready(attestation),
                },
                PendingOpen {
                    dlc, attestation, ..
                }
                | Open {
                    dlc, attestation, ..
                }
                | PendingCommit {
                    dlc, attestation, ..
                } => {
                    tracing::debug!(%order_id, "Was in unexpected state {}, jumping ahead to OpenCommitted", self.state);
                    CfdState::OpenCommitted {
                        common: CfdStateCommon {
                            transition_timestamp: Timestamp::now(),
                        },
                        dlc,
                        cet_status: match attestation {
                            None => CetStatus::TimelockExpired,
                            Some(attestation) => CetStatus::Ready(attestation),
                        },
                    }
                }
                _ => bail!(
                    "Cannot transition to OpenCommitted because of unexpected state {}",
                    self.state
                ),
            },
            monitor::Event::RefundTimelockExpired(_) => {
                let dlc = if let OpenCommitted { dlc, .. } = self.state.clone() {
                    dlc
                } else if let Open { dlc, .. } | PendingOpen { dlc, .. } = self.state.clone() {
                    tracing::debug!(%order_id, "Was in unexpected state {}, jumping ahead to PendingRefund", self.state);
                    dlc
                } else {
                    bail!(
                        "Cannot transition to PendingRefund because of unexpected state {}",
                        self.state
                    )
                };

                CfdState::must_refund(dlc)
            }
            monitor::Event::RefundFinality(_) => {
                let dlc = self
                    .dlc()
                    .context("No dlc available when reaching refund finality")?;

                CfdState::refunded(dlc)
            }
            monitor::Event::CetFinality(_) => {
                let attestation = self
                    .attestation()
                    .context("No attestation available when reaching CET finality")?;

                CfdState::closed(Payout::Cet(attestation))
            }
            monitor::Event::RevokedTransactionFound(_) => {
                todo!("Punish bad counterparty")
            }
        };

        self.state = new_state.clone();

        Ok(Some(new_state))
    }

    pub fn handle_commit_tx_sent(&mut self) -> Result<Option<CfdState>> {
        use CfdState::*;

        // early exit if already final
        if let SetupFailed { .. } | Closed { .. } | Refunded { .. } = self.state.clone() {
            tracing::trace!(
                "Ignoring sent commit transaction because cfd already in state {}",
                self.state
            );
            return Ok(None);
        }

        let (dlc, attestation) = match self.state.clone() {
            PendingOpen {
                dlc, attestation, ..
            } => (dlc, attestation),
            Open {
                dlc, attestation, ..
            } => (dlc, attestation),
            _ => {
                bail!(
                    "Cannot transition to PendingCommit because of unexpected state {}",
                    self.state
                )
            }
        };

        self.state = PendingCommit {
            common: CfdStateCommon {
                transition_timestamp: Timestamp::now(),
            },
            dlc,
            attestation,
        };

        Ok(Some(self.state.clone()))
    }

    pub fn handle_oracle_attestation(
        &mut self,
        attestation: Attestation,
    ) -> Result<Option<CfdState>> {
        use CfdState::*;

        // early exit if already final
        if let SetupFailed { .. } | Closed { .. } | Refunded { .. } = self.state.clone() {
            tracing::trace!(
                "Ignoring oracle attestation because cfd already in state {}",
                self.state
            );
            return Ok(None);
        }

        let new_state = match self.state.clone() {
            CfdState::PendingOpen { dlc, .. } => CfdState::PendingOpen {
                common: CfdStateCommon {
                    transition_timestamp: Timestamp::now(),
                },
                dlc,
                attestation: Some(attestation),
            },
            CfdState::Open { dlc, .. } => CfdState::Open {
                common: CfdStateCommon {
                    transition_timestamp: Timestamp::now(),
                },
                dlc,
                attestation: Some(attestation),
                collaborative_close: None,
            },
            CfdState::PendingCommit { dlc, .. } => CfdState::PendingCommit {
                common: CfdStateCommon {
                    transition_timestamp: Timestamp::now(),
                },
                dlc,
                attestation: Some(attestation),
            },
            CfdState::OpenCommitted {
                dlc,
                cet_status: CetStatus::Unprepared,
                ..
            } => CfdState::OpenCommitted {
                common: CfdStateCommon {
                    transition_timestamp: Timestamp::now(),
                },
                dlc,
                cet_status: CetStatus::OracleSigned(attestation),
            },
            CfdState::OpenCommitted {
                dlc,
                cet_status: CetStatus::TimelockExpired,
                ..
            } => CfdState::OpenCommitted {
                common: CfdStateCommon {
                    transition_timestamp: Timestamp::now(),
                },
                dlc,
                cet_status: CetStatus::Ready(attestation),
            },
            _ => bail!(
                "Cannot transition to OpenCommitted because of unexpected state {}",
                self.state
            ),
        };

        self.state = new_state.clone();

        Ok(Some(new_state))
    }

    pub fn handle_cet_sent(&mut self) -> Result<Option<CfdState>> {
        use CfdState::*;

        // early exit if already final
        if let SetupFailed { .. } | Closed { .. } | Refunded { .. } = self.state.clone() {
            tracing::trace!(
                "Ignoring pending CET because cfd already in state {}",
                self.state
            );
            return Ok(None);
        }

        let dlc = self.dlc().context("No DLC available after CET was sent")?;
        let attestation = self
            .attestation()
            .context("No attestation available after CET was sent")?;

        self.state = CfdState::PendingCet {
            common: CfdStateCommon {
                transition_timestamp: Timestamp::now(),
            },
            dlc,
            attestation,
        };

        Ok(Some(self.state.clone()))
    }

    pub fn handle_proposal_signed(
        &mut self,
        collaborative_close: CollaborativeSettlement,
    ) -> Result<Option<CfdState>> {
        use CfdState::*;

        // early exit if already final
        if let SetupFailed { .. } | Closed { .. } | Refunded { .. } = self.state.clone() {
            tracing::trace!(
                "Ignoring collaborative settlement because cfd already in state {}",
                self.state
            );
            return Ok(None);
        }

        let new_state = match self.state.clone() {
            CfdState::Open {
                common,
                dlc,
                attestation,
                ..
            } => CfdState::Open {
                common,
                dlc,
                attestation,
                collaborative_close: Some(collaborative_close),
            },
            _ => bail!(
                "Cannot add proposed settlement details to state because of unexpected state {}",
                self.state
            ),
        };

        self.state = new_state.clone();

        Ok(Some(new_state))
    }

    pub fn refund_tx(&self) -> Result<Transaction> {
        let dlc = if let CfdState::PendingRefund { dlc, .. } = self.state.clone() {
            dlc
        } else {
            bail!("Refund transaction can only be constructed when in state PendingRefund, but we are currently in {}", self.state.clone())
        };

        dlc.signed_refund_tx()
    }

    pub fn commit_tx(&self) -> Result<Transaction> {
        let dlc = if let CfdState::Open { dlc, .. }
        | CfdState::PendingOpen { dlc, .. }
        | CfdState::PendingCommit { dlc, .. } = self.state.clone()
        {
            dlc
        } else {
            bail!(
                "Cannot publish commit transaction in state {}",
                self.state.clone()
            )
        };

        dlc.signed_commit_tx()
    }

    pub fn cet(&self) -> Result<Result<Transaction, NotReadyYet>> {
        let (dlc, attestation) = match self.state.clone() {
            CfdState::OpenCommitted {
                dlc,
                cet_status: CetStatus::Ready(attestation),
                ..
            }
            | CfdState::PendingCet {
                dlc, attestation, ..
            } => (dlc, attestation),
            CfdState::OpenCommitted { cet_status, .. } => {
                return Ok(Err(NotReadyYet { cet_status }));
            }
            CfdState::Open { .. } | CfdState::PendingCommit { .. } => {
                return Ok(Err(NotReadyYet {
                    cet_status: CetStatus::Unprepared,
                }));
            }
            _ => bail!("Cannot publish CET in state {}", self.state.clone()),
        };

        let signed_cet = dlc.signed_cet(&attestation)?;

        Ok(Ok(signed_cet))
    }

    pub fn pending_open_dlc(&self) -> Option<Dlc> {
        if let CfdState::PendingOpen { dlc, .. } = self.state.clone() {
            Some(dlc)
        } else {
            None
        }
    }

    pub fn open_dlc(&self) -> Option<Dlc> {
        if let CfdState::Open { dlc, .. } = self.state.clone() {
            Some(dlc)
        } else {
            None
        }
    }

    pub fn is_must_refund(&self) -> bool {
        matches!(self.state.clone(), CfdState::PendingRefund { .. })
    }

    pub fn is_pending_commit(&self) -> bool {
        matches!(self.state.clone(), CfdState::PendingCommit { .. })
    }

    pub fn is_pending_cet(&self) -> bool {
        matches!(self.state.clone(), CfdState::PendingCet { .. })
    }

    pub fn is_cleanup(&self) -> bool {
        matches!(
            self.state.clone(),
            CfdState::OutgoingOrderRequest { .. }
                | CfdState::IncomingOrderRequest { .. }
                | CfdState::Accepted { .. }
                | CfdState::ContractSetup { .. }
        )
    }

    pub fn is_collaborative_settle_possible(&self) -> bool {
        matches!(self.state.clone(), CfdState::Open { .. })
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn dlc(&self) -> Option<Dlc> {
        match self.state.clone() {
            CfdState::PendingOpen { dlc, .. }
            | CfdState::Open { dlc, .. }
            | CfdState::PendingCommit { dlc, .. }
            | CfdState::OpenCommitted { dlc, .. }
            | CfdState::PendingRefund { dlc, .. }
            | CfdState::PendingCet { dlc, .. } => Some(dlc),

            CfdState::OutgoingOrderRequest { .. }
            | CfdState::IncomingOrderRequest { .. }
            | CfdState::Accepted { .. }
            | CfdState::Rejected { .. }
            | CfdState::ContractSetup { .. }
            | CfdState::Closed { .. }
            | CfdState::Refunded { .. }
            | CfdState::SetupFailed { .. } => None,
        }
    }

    fn attestation(&self) -> Option<Attestation> {
        match self.state.clone() {
            CfdState::PendingOpen {
                attestation: Some(attestation),
                ..
            }
            | CfdState::Open {
                attestation: Some(attestation),
                ..
            }
            | CfdState::PendingCommit {
                attestation: Some(attestation),
                ..
            }
            | CfdState::OpenCommitted {
                cet_status: CetStatus::OracleSigned(attestation) | CetStatus::Ready(attestation),
                ..
            }
            | CfdState::PendingCet { attestation, .. }
            | CfdState::Closed {
                payout: Payout::Cet(attestation),
                ..
            } => Some(attestation),

            CfdState::OutgoingOrderRequest { .. }
            | CfdState::IncomingOrderRequest { .. }
            | CfdState::Accepted { .. }
            | CfdState::Rejected { .. }
            | CfdState::ContractSetup { .. }
            | CfdState::PendingOpen { .. }
            | CfdState::Open { .. }
            | CfdState::PendingCommit { .. }
            | CfdState::Closed { .. }
            | CfdState::OpenCommitted { .. }
            | CfdState::PendingRefund { .. }
            | CfdState::Refunded { .. }
            | CfdState::SetupFailed { .. } => None,
        }
    }

    pub fn collaborative_close(&self) -> Option<CollaborativeSettlement> {
        match self.state.clone() {
            CfdState::Open {
                collaborative_close: Some(collaborative_close),
                ..
            }
            | CfdState::Closed {
                payout: Payout::CollaborativeClose(collaborative_close),
                ..
            } => Some(collaborative_close),

            CfdState::OutgoingOrderRequest { .. }
            | CfdState::IncomingOrderRequest { .. }
            | CfdState::Accepted { .. }
            | CfdState::Rejected { .. }
            | CfdState::ContractSetup { .. }
            | CfdState::PendingOpen { .. }
            | CfdState::Open { .. }
            | CfdState::PendingCommit { .. }
            | CfdState::PendingCet { .. }
            | CfdState::Closed { .. }
            | CfdState::OpenCommitted { .. }
            | CfdState::PendingRefund { .. }
            | CfdState::Refunded { .. }
            | CfdState::SetupFailed { .. } => None,
        }
    }

    /// Returns the payout of the Cfd
    ///
    /// In case the cfd's payout is not fixed yet (because we don't have attestation or
    /// collaborative close transaction None is returned, which means that the payout is still
    /// undecided
    pub fn payout(&self) -> Option<Amount> {
        // early exit in case of refund scenario
        if let CfdState::PendingRefund { dlc, .. } | CfdState::Refunded { dlc, .. } =
            self.state.clone()
        {
            return Some(dlc.refund_amount(self.role()));
        }

        // decision between attestation and collaborative close payout
        match (self.attestation(), self.collaborative_close()) {
            (Some(_attestation), Some(collaborative_close)) => Some(collaborative_close.payout()),
            (None, Some(collaborative_close)) => Some(collaborative_close.payout()),
            (Some(attestation), None) => Some(attestation.payout()),
            (None, None) => None,
        }
    }

    /// Only cfds in state `Open` that have not received an attestation and are within 23 hours
    /// until expiry are eligible for rollover
    pub fn can_roll_over(&self, now: OffsetDateTime) -> Result<(), CannotRollover> {
        let expiry_timestamp = self.expiry_timestamp().ok_or(CannotRollover::NoDlc)?;

        if now > expiry_timestamp {
            return Err(CannotRollover::AlreadyExpired);
        }

        let time_until_expiry = expiry_timestamp - now;

        if time_until_expiry > SETTLEMENT_INTERVAL - Duration::HOUR {
            return Err(CannotRollover::WasJustRolledOver);
        }

        // only state open with no attestation is acceptable for rollover
        if !matches!(
            self.state.clone(),
            CfdState::Open {
                attestation: None,
                ..
            }
        ) {
            return Err(CannotRollover::WrongState {
                state: self.state.to_string(),
            });
        }

        Ok(())
    }

    pub fn id(&self) -> OrderId {
        self.id
    }

    pub fn trading_pair(&self) -> TradingPair {
        self.trading_pair
    }

    pub fn price(&self) -> Price {
        self.price
    }

    pub fn leverage(&self) -> Leverage {
        self.leverage
    }

    pub fn liquidation_price(&self) -> Price {
        self.liquidation_price
    }

    pub fn creation_timestamp(&self) -> Timestamp {
        self.creation_timestamp
    }

    pub fn settlement_interval(&self) -> Duration {
        self.settlement_interval
    }

    pub fn fee_rate(&self) -> u32 {
        self.fee_rate
    }

    pub fn quantity_usd(&self) -> Usd {
        self.quantity_usd
    }

    pub fn state(&self) -> &CfdState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut CfdState {
        &mut self.state
    }

    pub fn counterparty(&self) -> Identity {
        self.counterparty
    }
}

#[derive(thiserror::Error, Debug, PartialEq)]
pub enum CannotRollover {
    #[error("Cfd does not have a dlc")]
    NoDlc,
    #[error("The Cfd is already expired")]
    AlreadyExpired,
    #[error("The Cfd was just rolled over")]
    WasJustRolledOver,
    #[error("Cannot roll over in state {state}")]
    WrongState { state: String },
}

#[derive(thiserror::Error, Debug, Clone)]
#[error("The cfd is not ready for CET publication yet: {cet_status}")]
pub struct NotReadyYet {
    cet_status: CetStatus,
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

/// Calculates the long's margin in BTC
///
/// The margin is the initial margin and represents the collateral the buyer
/// has to come up with to satisfy the contract. Here we calculate the initial
/// long margin as: quantity / (initial_price * leverage)
pub fn calculate_long_margin(price: Price, quantity: Usd, leverage: Leverage) -> Amount {
    quantity / (price * leverage)
}

/// Calculates the shorts's margin in BTC
///
/// The short margin is represented as the quantity of the contract given the
/// initial price. The short side can currently not leverage the position but
/// always has to cover the complete quantity.
fn calculate_short_margin(price: Price, quantity: Usd) -> Amount {
    quantity / price
}

fn calculate_long_liquidation_price(leverage: Leverage, price: Price) -> Price {
    price * leverage / (leverage + 1)
}

// PLACEHOLDER
// fn calculate_short_liquidation_price(leverage: Leverage, price: Price) -> Price {
//     price * leverage / (leverage - 1)
// }

/// Returns the Profit/Loss (P/L) as Bitcoin. Losses are capped by the provided margin
fn calculate_profit(
    initial_price: Price,
    closing_price: Price,
    quantity: Usd,
    leverage: Leverage,
    position: Position,
) -> Result<(SignedAmount, Percent)> {
    let inv_initial_price =
        InversePrice::new(initial_price).context("cannot invert invalid price")?;
    let inv_closing_price =
        InversePrice::new(closing_price).context("cannot invert invalid price")?;
    let long_liquidation_price = calculate_long_liquidation_price(leverage, initial_price);
    let long_is_liquidated = closing_price <= long_liquidation_price;

    let long_margin = calculate_long_margin(initial_price, quantity, leverage)
        .to_signed()
        .context("Unable to compute long margin")?;
    let short_margin = calculate_short_margin(initial_price, quantity)
        .to_signed()
        .context("Unable to compute short margin")?;
    let amount_changed = (quantity * inv_initial_price)
        .to_signed()
        .context("Unable to convert to SignedAmount")?
        - (quantity * inv_closing_price)
            .to_signed()
            .context("Unable to convert to SignedAmount")?;

    // calculate profit/loss (P and L) in BTC
    let (margin, payout) = match position {
        // TODO:
        // At this point, long_leverage == leverage, short_leverage == 1
        // which has the effect that the right boundary `b` below is
        // infinite and not used.
        //
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
            let payout = match long_is_liquidated {
                true => SignedAmount::ZERO,
                false => long_margin + amount_changed,
            };
            (long_margin, payout)
        }
        Position::Short => {
            let payout = match long_is_liquidated {
                true => long_margin + short_margin,
                false => short_margin - amount_changed,
            };
            (short_margin, payout)
        }
    };

    let profit = payout - margin;
    let percent = Decimal::from_f64(100. * profit.as_sat() as f64 / margin.as_sat() as f64)
        .context("Unable to compute percent")?;

    Ok((profit, Percent(percent)))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cet {
    pub tx: Transaction,
    pub adaptor_sig: EcdsaAdaptorSignature,

    // TODO: Range + number of digits (usize) could be represented as Digits similar to what we do
    // in the protocol lib
    pub range: RangeInclusive<u64>,
    pub n_bits: usize,
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
}

impl Dlc {
    /// Create a close transaction based on the current contract and a settlement proposals
    pub fn close_transaction(
        &self,
        proposal: &crate::model::cfd::SettlementProposal,
    ) -> Result<(Transaction, Signature)> {
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
        )
        .context("Unable to collaborative close transaction")?;

        let sig = SECP256K1.sign(&sighash, &self.identity);

        Ok((tx, sig))
    }

    pub fn finalize_spend_transaction(
        &self,
        (close_tx, own_sig): (Transaction, Signature),
        counterparty_sig: Signature,
    ) -> Result<Transaction> {
        let own_pk = PublicKey::new(secp256k1_zkp::PublicKey::from_secret_key(
            SECP256K1,
            &self.identity,
        ));

        let (_, lock_desc) = &self.lock;
        let spend_tx = maia::finalize_spend_transaction(
            close_tx,
            lock_desc,
            (own_pk, own_sig),
            (self.identity_counterparty, counterparty_sig),
        )?;

        Ok(spend_tx)
    }

    pub fn refund_amount(&self, role: Role) -> Amount {
        let our_script_pubkey = match role {
            Role::Taker => self.taker_address.script_pubkey(),
            Role::Maker => self.maker_address.script_pubkey(),
        };

        self.refund
            .0
            .output
            .iter()
            .find(|output| output.script_pubkey == our_script_pubkey)
            .map(|output| Amount::from_sat(output.value))
            .unwrap_or_default()
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
        let signed_refund_tx = finalize_spend_transaction(
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

        let signed_commit_tx = finalize_spend_transaction(
            self.commit.0.clone(),
            &self.lock.1,
            (our_pubkey, our_sig),
            (counterparty_pubkey, counterparty_sig),
        )?;

        Ok(signed_commit_tx)
    }

    pub fn signed_cet(&self, attestation: &Attestation) -> Result<Transaction> {
        let cets = self
            .cets
            .get(&attestation.id)
            .context("Unable to find oracle event id within the cets of the self")?;

        let Cet {
            tx: cet,
            adaptor_sig: encsig,
            n_bits,
            ..
        } = cets
            .iter()
            .find(|Cet { range, .. }| range.contains(&attestation.price))
            .context("Price out of range of cets")?;

        let mut decryption_sk = attestation.scalars[0];
        for oracle_attestation in attestation.scalars[1..*n_bits].iter() {
            decryption_sk.add_assign(oracle_attestation.as_ref())?;
        }

        let sig_hash = spending_tx_sighash(
            cet,
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

        let signed_cet = finalize_spend_transaction(
            cet.clone(),
            &self.commit.2,
            (our_pubkey, our_sig),
            (counterparty_pubkey, counterparty_sig),
        )?;

        Ok(signed_cet)
    }
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
            timestamp: Timestamp::now(),
            payout,
            price,
        })
    }

    pub fn payout(&self) -> Amount {
        self.payout
    }
}

#[allow(clippy::large_enum_variant)]
pub enum Completed<P> {
    Succeeded {
        order_id: OrderId,
        payload: P,
    },
    Rejected {
        order_id: OrderId,
        reason: anyhow::Error,
    },
    Failed {
        order_id: OrderId,
        error: anyhow::Error,
    },
}

pub mod marker {
    /// Marker type for contract setup completion
    pub struct Setup;
    /// Marker type for rollover  completion
    pub struct Rollover;
}

/// Message sent from a setup actor to the
/// cfd actor to notify that the contract setup has finished.
pub type SetupCompleted = Completed<(Dlc, marker::Setup)>;

/// Message sent from a rollover actor to the
/// cfd actor to notify that the rollover has finished (contract got updated).
/// TODO: Roll it out in the maker rollover actor
pub type RolloverCompleted = Completed<(Dlc, marker::Rollover)>;

impl Completed<(Dlc, marker::Setup)> {
    pub fn succeeded(order_id: OrderId, dlc: Dlc) -> Self {
        Self::Succeeded {
            order_id,
            payload: (dlc, marker::Setup),
        }
    }
}

impl Completed<(Dlc, marker::Rollover)> {
    pub fn succeeded(order_id: OrderId, dlc: Dlc) -> Self {
        Self::Succeeded {
            order_id,
            payload: (dlc, marker::Rollover),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed::Seed;
    use bdk::bitcoin::util::psbt::Global;
    use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;
    use std::str::FromStr;
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

        let long_margin = calculate_long_margin(price, quantity, leverage);

        assert_eq!(long_margin, Amount::ONE_BTC);
    }

    #[test]
    fn given_leverage_of_one_and_leverage_of_ten_then_long_margin_is_lower_factor_ten() {
        let price = Price::new(dec!(40000)).unwrap();
        let quantity = Usd::new(dec!(40000));
        let leverage = Leverage::new(10).unwrap();

        let long_margin = calculate_long_margin(price, quantity, leverage);

        assert_eq!(long_margin, Amount::from_btc(0.1).unwrap());
    }

    #[test]
    fn given_quantity_equals_price_then_short_margin_is_one_btc() {
        let price = Price::new(dec!(40000)).unwrap();
        let quantity = Usd::new(dec!(40000));

        let short_margin = calculate_short_margin(price, quantity);

        assert_eq!(short_margin, Amount::ONE_BTC);
    }

    #[test]
    fn given_quantity_half_of_price_then_short_margin_is_half_btc() {
        let price = Price::new(dec!(40000)).unwrap();
        let quantity = Usd::new(dec!(20000));

        let short_margin = calculate_short_margin(price, quantity);

        assert_eq!(short_margin, Amount::from_btc(0.5).unwrap());
    }

    #[test]
    fn given_quantity_double_of_price_then_short_margin_is_two_btc() {
        let price = Price::new(dec!(40000)).unwrap();
        let quantity = Usd::new(dec!(80000));

        let short_margin = calculate_short_margin(price, quantity);

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
        assert_profit_loss_values(
            Price::new(dec!(10_000)).unwrap(),
            Price::new(dec!(10_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::new(2).unwrap(),
            Position::Long,
            SignedAmount::ZERO,
            Decimal::ZERO.into(),
            "No price increase means no profit",
        );

        assert_profit_loss_values(
            Price::new(dec!(10_000)).unwrap(),
            Price::new(dec!(20_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::new(2).unwrap(),
            Position::Long,
            SignedAmount::from_sat(50_000_000),
            dec!(100).into(),
            "A price increase of 2x should result in a profit of 100% (long)",
        );

        assert_profit_loss_values(
            Price::new(dec!(9_000)).unwrap(),
            Price::new(dec!(6_000)).unwrap(),
            Usd::new(dec!(9_000)),
            Leverage::new(2).unwrap(),
            Position::Long,
            SignedAmount::from_sat(-50_000_000),
            dec!(-100).into(),
            "A price drop of 1/(Leverage + 1) x should result in 100% loss (long)",
        );

        assert_profit_loss_values(
            Price::new(dec!(10_000)).unwrap(),
            Price::new(dec!(5_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::new(2).unwrap(),
            Position::Long,
            SignedAmount::from_sat(-50_000_000),
            dec!(-100).into(),
            "A loss should be capped at 100% (long)",
        );

        assert_profit_loss_values(
            Price::new(dec!(50_400)).unwrap(),
            Price::new(dec!(60_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::new(2).unwrap(),
            Position::Long,
            SignedAmount::from_sat(3_174_603),
            dec!(31.99999798400001).into(),
            "long position should make a profit when price goes up",
        );

        assert_profit_loss_values(
            Price::new(dec!(50_400)).unwrap(),
            Price::new(dec!(60_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::new(2).unwrap(),
            Position::Short,
            SignedAmount::from_sat(-3_174_603),
            dec!(-15.99999899200001).into(),
            "short position should make a loss when price goes up",
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn assert_profit_loss_values(
        initial_price: Price,
        current_price: Price,
        quantity: Usd,
        leverage: Leverage,
        position: Position,
        should_profit: SignedAmount,
        should_profit_in_percent: Percent,
        msg: &str,
    ) {
        let (profit, in_percent) =
            calculate_profit(initial_price, current_price, quantity, leverage, position).unwrap();

        assert_eq!(profit, should_profit, "{}", msg);
        assert_eq!(in_percent, should_profit_in_percent, "{}", msg);
    }

    #[test]
    fn test_profit_calculation_loss_plus_profit_should_be_zero() {
        let initial_price = Price::new(dec!(10_000)).unwrap();
        let closing_price = Price::new(dec!(16_000)).unwrap();
        let quantity = Usd::new(dec!(10_000));
        let leverage = Leverage::new(1).unwrap();
        let (profit, profit_in_percent) = calculate_profit(
            initial_price,
            closing_price,
            quantity,
            leverage,
            Position::Long,
        )
        .unwrap();
        let (loss, loss_in_percent) = calculate_profit(
            initial_price,
            closing_price,
            quantity,
            leverage,
            Position::Short,
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
        let leverage = Leverage::new(2).unwrap();
        let long_margin = calculate_long_margin(initial_price, quantity, leverage)
            .to_signed()
            .unwrap();
        let short_margin = calculate_short_margin(initial_price, quantity)
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

        for price in closing_prices {
            let (long_profit, _) =
                calculate_profit(initial_price, price, quantity, leverage, Position::Long).unwrap();
            let (short_profit, _) =
                calculate_profit(initial_price, price, quantity, leverage, Position::Short)
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

    #[tokio::test]
    async fn given_cfd_expires_now_then_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //                                                          now

        let cfd = Cfd::dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));
        let result = cfd.can_roll_over(datetime!(2021-11-19 10:00:00).assume_utc());

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn given_cfd_expires_within_23hours_then_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //        now

        let cfd = Cfd::dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));

        let result = cfd.can_roll_over(datetime!(2021-11-18 11:00:00).assume_utc());

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn given_cfd_past_expiry_time_then_no_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //                                                           now

        let cfd = Cfd::dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));
        let cannot_roll_over = cfd
            .can_roll_over(datetime!(2021-11-19 10:00:01).assume_utc())
            .unwrap_err();

        assert_eq!(cannot_roll_over, CannotRollover::AlreadyExpired)
    }

    #[tokio::test]
    async fn given_cfd_was_just_rolled_over_then_no_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //    now

        let cfd = Cfd::dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));
        let cannot_roll_over = cfd
            .can_roll_over(datetime!(2021-11-18 10:00:01).assume_utc())
            .unwrap_err();

        assert_eq!(cannot_roll_over, CannotRollover::WasJustRolledOver)
    }

    #[tokio::test]
    async fn given_cfd_out_of_bounds_expiry_then_no_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //  now

        let cfd = Cfd::dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));
        let cannot_roll_over = cfd
            .can_roll_over(datetime!(2021-11-18 09:59:59).assume_utc())
            .unwrap_err();

        assert_eq!(cannot_roll_over, CannotRollover::WasJustRolledOver)
    }

    #[tokio::test]
    async fn given_cfd_was_renewed_less_than_1h_ago_then_no_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //       now

        let cfd = Cfd::dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));
        let cannot_roll_over = cfd
            .can_roll_over(datetime!(2021-11-18 10:59:59).assume_utc())
            .unwrap_err();

        assert_eq!(cannot_roll_over, CannotRollover::WasJustRolledOver)
    }

    #[tokio::test]
    async fn given_cfd_has_attestation_then_no_rollover() {
        let cfd = Cfd::dummy_open_with_attestation(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));

        let cannot_roll_over = cfd
            .can_roll_over(datetime!(2021-11-19 10:00:00).assume_utc())
            .unwrap_err();

        assert!(matches!(
            cannot_roll_over,
            CannotRollover::WrongState { .. }
        ))
    }

    #[tokio::test]
    async fn given_cfd_not_in_open_then_no_rollover() {
        let cfd = Cfd::dummy_not_open_but_dlc(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));

        let cannot_roll_over = cfd
            .can_roll_over(datetime!(2021-11-19 10:00:00).assume_utc())
            .unwrap_err();

        assert!(matches!(
            cannot_roll_over,
            CannotRollover::WrongState { .. }
        ))
    }

    impl Cfd {
        fn dummy_open(event_id: BitMexPriceEventId) -> Self {
            Cfd::from_order(
                Order::dummy_model(),
                Usd::new(dec!(1000)),
                CfdState::Open {
                    common: Default::default(),
                    dlc: Dlc::dummy(Some(event_id)),
                    attestation: None,
                    collaborative_close: None,
                },
                dummy_identity(),
                Role::Taker,
            )
        }

        fn dummy_open_with_attestation(event_id: BitMexPriceEventId) -> Self {
            Cfd::from_order(
                Order::dummy_model(),
                Usd::new(dec!(1000)),
                CfdState::Open {
                    common: Default::default(),
                    dlc: Dlc::dummy(Some(event_id)),
                    // the dummy_dlc contains a dummy range [0, 1]
                    attestation: Some(
                        Attestation::new(
                            BitMexPriceEventId::with_20_digits(OffsetDateTime::now_utc()),
                            0,
                            vec![],
                            Dlc::dummy(None),
                            Role::Taker,
                        )
                        .unwrap(),
                    ),
                    collaborative_close: None,
                },
                dummy_identity(),
                Role::Taker,
            )
        }

        fn dummy_not_open_but_dlc(event_id: BitMexPriceEventId) -> Self {
            Cfd::from_order(
                Order::dummy_model(),
                Usd::new(dec!(1000)),
                CfdState::PendingRefund {
                    common: Default::default(),
                    dlc: Dlc::dummy(Some(event_id)),
                },
                dummy_identity(),
                Role::Taker,
            )
        }
    }

    impl Order {
        fn dummy_model() -> Self {
            Order::new_short(
                Price::new(dec!(1000)).unwrap(),
                Usd::new(dec!(100)),
                Usd::new(dec!(1000)),
                Origin::Theirs,
                dummy_event_id(),
                time::Duration::hours(24),
                1,
            )
            .unwrap()
        }
    }

    impl Dlc {
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
                    tx: dummy_tx.clone(),
                    adaptor_sig: dummy_adapter_sig,
                    range: RangeInclusive::new(0, 1),
                    n_bits: 0,
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
            }
        }
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
        Identity::new(Seed::default().derive_identity().0)
    }

    pub fn dummy_event_id() -> BitMexPriceEventId {
        BitMexPriceEventId::with_20_digits(OffsetDateTime::now_utc())
    }
}
