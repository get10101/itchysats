use crate::model::{Leverage, Position, TradingPair, Usd};
use anyhow::Result;
use bdk::bitcoin::secp256k1::{SecretKey, Signature};
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::{Amount, Transaction};
use cfd_protocol::secp256k1_zkp::{schnorrsig, EcdsaAdaptorSignature};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderId(Uuid);

impl Default for OrderId {
    fn default() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Display for OrderId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Origin {
    Ours,
    Theirs,
}

/// A concrete order created by a maker for a taker
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Order {
    pub id: OrderId,

    pub trading_pair: TradingPair,
    pub position: Position,

    pub price: Usd,

    // TODO: [post-MVP] Representation of the contract size; at the moment the contract size is
    //  always 1 USD
    pub min_quantity: Usd,
    pub max_quantity: Usd,

    // TODO: [post-MVP] - Once we have multiple leverage we will have to move leverage and
    //  liquidation_price into the CFD and add a calculation endpoint for the taker buy screen
    pub leverage: Leverage,
    pub liquidation_price: Usd,

    pub creation_timestamp: SystemTime,

    /// The duration that will be used for calculating the settlement timestamp
    pub term: Duration,

    pub origin: Origin,
}

#[allow(dead_code)] // Only one binary and the tests use this.
impl Order {
    pub fn from_default_with_price(price: Usd, origin: Origin) -> Result<Self> {
        let leverage = Leverage(5);
        let maintenance_margin_rate = dec!(0.005);
        let liquidation_price =
            calculate_liquidation_price(&leverage, &price, &maintenance_margin_rate)?;

        Ok(Order {
            id: OrderId::default(),
            price,
            min_quantity: Usd(dec!(1000)),
            max_quantity: Usd(dec!(10000)),
            leverage,
            trading_pair: TradingPair::BtcUsd,
            liquidation_price,
            position: Position::Sell,
            creation_timestamp: SystemTime::now(),
            term: Duration::from_secs(60 * 60 * 8), // 8 hours
            origin,
        })
    }
    pub fn with_min_quantity(mut self, min_quantity: Usd) -> Order {
        self.min_quantity = min_quantity;
        self
    }

    pub fn with_max_quantity(mut self, max_quantity: Usd) -> Order {
        self.max_quantity = max_quantity;
        self
    }
}

fn calculate_liquidation_price(
    leverage: &Leverage,
    price: &Usd,
    maintenance_margin_rate: &Decimal,
) -> Result<Usd> {
    let leverage = Decimal::from(leverage.0).into();
    let maintenance_margin_rate: Usd = (*maintenance_margin_rate).into();

    // liquidation price calc in isolated margin mode
    // currently based on: https://help.bybit.com/hc/en-us/articles/360039261334-How-to-calculate-Liquidation-Price-Inverse-Contract-
    let liquidation_price = price.checked_mul(leverage)?.checked_div(
        leverage
            .checked_add(Decimal::ONE.into())?
            .checked_sub(maintenance_margin_rate.checked_mul(leverage)?)?,
    )?;

    Ok(liquidation_price)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Error {
    // TODO
    ConnectionLost,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CfdStateError {
    last_successful_state: CfdState,
    error: Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct CfdStateCommon {
    pub transition_timestamp: SystemTime,
}

// Note: De-/Serialize with type tag to make handling on UI easier
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum CfdState {
    /// The taker has requested to take a CFD, but has not messaged the maker yet.
    ///
    /// This state only applies to the taker.
    TakeRequested { common: CfdStateCommon },
    /// The taker sent an open request to the maker to open the CFD but don't have a response yet.
    ///
    /// This state applies to taker and maker.
    /// Initial state for the maker.
    PendingTakeRequest { common: CfdStateCommon },
    /// The maker has accepted the CFD take request, but the contract is not set up on chain yet.
    ///
    /// This state applies to taker and maker.
    Accepted { common: CfdStateCommon },

    /// The maker rejected the CFD take request.
    ///
    /// This state applies to taker and maker.
    Rejected { common: CfdStateCommon },

    /// State used during contract setup.
    ///
    /// This state applies to taker and maker.
    /// All contract setup messages between taker and maker are expected to be sent in on scope.
    ContractSetup { common: CfdStateCommon },

    /// The CFD contract is set up on chain.
    ///
    /// This state applies to taker and maker.
    Open {
        common: CfdStateCommon,
        settlement_timestamp: SystemTime,
        #[serde(with = "::bdk::bitcoin::util::amount::serde::as_sat")]
        margin: Amount,
    },

    /// Requested close the position, but we have not passed that on to the blockchain yet.
    ///
    /// This state applies to taker and maker.
    CloseRequested { common: CfdStateCommon },
    /// The close transaction (CET) was published on the Bitcoin blockchain but we don't have a
    /// confirmation yet.
    ///
    /// This state applies to taker and maker.
    PendingClose { common: CfdStateCommon },
    /// The close transaction is confirmed with at least one block.
    ///
    /// This state applies to taker and maker.
    Closed { common: CfdStateCommon },
    /// Error state
    ///
    /// This state applies to taker and maker.
    Error { common: CfdStateCommon },
}

impl CfdState {
    fn get_common(&self) -> CfdStateCommon {
        let common = match self {
            CfdState::TakeRequested { common } => common,
            CfdState::PendingTakeRequest { common } => common,
            CfdState::Accepted { common } => common,
            CfdState::Rejected { common } => common,
            CfdState::ContractSetup { common } => common,
            CfdState::Open { common, .. } => common,
            CfdState::CloseRequested { common } => common,
            CfdState::PendingClose { common } => common,
            CfdState::Closed { common } => common,
            CfdState::Error { common } => common,
        };

        *common
    }

    pub fn get_transition_timestamp(&self) -> SystemTime {
        self.get_common().transition_timestamp
    }
}

impl Display for CfdState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            CfdState::TakeRequested { .. } => {
                write!(f, "Take Requested")
            }
            CfdState::PendingTakeRequest { .. } => {
                write!(f, "Pending Take Request")
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
            CfdState::Open { .. } => {
                write!(f, "Open")
            }
            CfdState::CloseRequested { .. } => {
                write!(f, "Close Requested")
            }
            CfdState::PendingClose { .. } => {
                write!(f, "Pending Close")
            }
            CfdState::Closed { .. } => {
                write!(f, "Closed")
            }
            CfdState::Error { .. } => {
                write!(f, "Error")
            }
        }
    }
}

/// Represents a cfd (including state)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cfd {
    pub order: Order,
    pub quantity_usd: Usd,
    pub state: CfdState,
    /* TODO: Leverage is currently derived from the Order, but the actual leverage should be
     * stored in the Cfd once there is multiple choices of leverage */
}

impl Cfd {
    pub fn new(order: Order, quantity: Usd, state: CfdState) -> Self {
        Cfd {
            order,
            quantity_usd: quantity,
            state,
        }
    }

    pub fn margin(&self) -> Result<Amount> {
        let margin = match self.position() {
            Position::Buy => {
                calculate_buy_margin(self.order.price, self.quantity_usd, self.order.leverage)?
            }
            Position::Sell => calculate_sell_margin(self.order.price, self.quantity_usd)?,
        };

        Ok(margin)
    }

    pub fn counterparty_margin(&self) -> Result<Amount> {
        let margin = match self.position() {
            Position::Buy => calculate_sell_margin(self.order.price, self.quantity_usd)?,
            Position::Sell => {
                calculate_buy_margin(self.order.price, self.quantity_usd, self.order.leverage)?
            }
        };

        Ok(margin)
    }

    pub fn profit(&self, current_price: Usd) -> Result<(Amount, Usd)> {
        let profit =
            calculate_profit(self.order.price, current_price, dec!(0.005), Usd(dec!(0.1)))?;
        Ok(profit)
    }

    pub fn position(&self) -> Position {
        match self.order.origin {
            Origin::Ours => self.order.position.clone(),

            // If the order is not our own we take the counter-position in the CFD
            Origin::Theirs => match self.order.position {
                Position::Buy => Position::Sell,
                Position::Sell => Position::Buy,
            },
        }
    }

    #[allow(dead_code)]
    pub fn refund_timelock_in_blocks(&self) -> u32 {
        self.order
            .term
            .mul_f32(Cfd::REFUND_THRESHOLD)
            .as_blocks()
            .ceil() as u32
    }

    /// A factor to be added to the CFD order term for calculating the refund timelock.
    ///
    /// The refund timelock is important in case the oracle disappears or never publishes a
    /// signature. Ideally, both users collaboratively settle in the refund scenario. This
    /// factor is important if the users do not settle collaboratively.
    /// `1.5` times the term as defined in CFD order should be safe in the extreme case where a user
    /// publishes the commit transaction right after the contract was initialized. In this case, the
    /// oracle still has `1.0 * cfdorder.term` time to attest and no one can publish the refund
    /// transaction.
    /// The downside is that if the oracle disappears: the users would only notice at the end
    /// of the cfd term. In this case the users has to wait for another `1.5` times of the
    /// term to get his funds back.
    #[allow(dead_code)]
    const REFUND_THRESHOLD: f32 = 1.5;
}

fn calculate_profit(
    _intial_price: Usd,
    _current_price: Usd,
    _interest_per_day: Decimal,
    _fee: Usd,
) -> Result<(Amount, Usd)> {
    // TODO: profit calculation
    Ok((Amount::ZERO, Usd::ZERO))
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
        self.as_secs_f32() / 60.0 / 10.0
    }
}

/// Calculates the buyer's margin in BTC
///
/// The margin is the initial margin and represents the collateral the buyer has to come up with to
/// satisfy the contract. Here we calculate the initial buy margin as: quantity / (initial_price *
/// leverage)
pub fn calculate_buy_margin(price: Usd, quantity: Usd, leverage: Leverage) -> Result<Amount> {
    let leverage = Decimal::from(leverage.0).into();

    let margin = quantity.checked_div(price.checked_mul(leverage)?)?;

    let sat_adjust = Decimal::from(Amount::ONE_BTC.as_sat()).into();
    let margin = margin.checked_mul(sat_adjust)?;
    let margin = Amount::from_sat(margin.try_into_u64()?);

    Ok(margin)
}

/// Calculates the seller's margin in BTC
///
/// The seller margin is represented as the quantity of the contract given the initial price.
/// The seller can currently not leverage the position but always has to cover the complete
/// quantity.
fn calculate_sell_margin(price: Usd, quantity: Usd) -> Result<Amount> {
    let margin = quantity.checked_div(price)?;

    let sat_adjust = Decimal::from(Amount::ONE_BTC.as_sat()).into();
    let margin = margin.checked_mul(sat_adjust)?;
    let margin = Amount::from_sat(margin.try_into_u64()?);

    Ok(margin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use std::time::UNIX_EPOCH;

    #[test]
    fn given_default_values_then_expected_liquidation_price() {
        let leverage = Leverage(5);
        let price = Usd(dec!(49000));
        let maintenance_margin_rate = dec!(0.005);

        let liquidation_price =
            calculate_liquidation_price(&leverage, &price, &maintenance_margin_rate).unwrap();

        assert_eq!(liquidation_price, Usd(dec!(41004.184100418410041841004184)));
    }

    #[test]
    fn given_leverage_of_one_and_equal_price_and_quantity_then_buy_margin_is_one_btc() {
        let price = Usd(dec!(40000));
        let quantity = Usd(dec![40000]);
        let leverage = Leverage(1);

        let buy_margin = calculate_buy_margin(price, quantity, leverage).unwrap();

        assert_eq!(buy_margin, Amount::ONE_BTC);
    }

    #[test]
    fn given_leverage_of_one_and_leverage_of_ten_then_buy_margin_is_lower_factor_ten() {
        let price = Usd(dec!(40000));
        let quantity = Usd(dec![40000]);
        let leverage = Leverage(10);

        let buy_margin = calculate_buy_margin(price, quantity, leverage).unwrap();

        assert_eq!(buy_margin, Amount::from_btc(0.1).unwrap());
    }

    #[test]
    fn given_quantity_equals_price_then_sell_margin_is_one_btc() {
        let price = Usd(dec!(40000));
        let quantity = Usd(dec![40000]);

        let sell_margin = calculate_sell_margin(price, quantity).unwrap();

        assert_eq!(sell_margin, Amount::ONE_BTC);
    }

    #[test]
    fn given_quantity_half_of_price_then_sell_margin_is_half_btc() {
        let price = Usd(dec!(40000));
        let quantity = Usd(dec![20000]);

        let sell_margin = calculate_sell_margin(price, quantity).unwrap();

        assert_eq!(sell_margin, Amount::from_btc(0.5).unwrap());
    }

    #[test]
    fn given_quantity_double_of_price_then_sell_margin_is_two_btc() {
        let price = Usd(dec!(40000));
        let quantity = Usd(dec![80000]);

        let sell_margin = calculate_sell_margin(price, quantity).unwrap();

        assert_eq!(sell_margin, Amount::from_btc(2.0).unwrap());
    }

    #[test]
    fn serialize_cfd_state_snapshot() {
        // This test is to prevent us from breaking the CfdState API against the database.
        // We serialize the state into the database, so changes to the enum result in breaking
        // program version changes.

        let fixed_timestamp = UNIX_EPOCH;

        let cfd_state = CfdState::TakeRequested {
            common: CfdStateCommon {
                transition_timestamp: fixed_timestamp,
            },
        };
        let json = serde_json::to_string(&cfd_state).unwrap();
        assert_eq!(
            json,
            r#"{"type":"TakeRequested","payload":{"common":{"transition_timestamp":{"secs_since_epoch":0,"nanos_since_epoch":0}}}}"#
        );

        let cfd_state = CfdState::PendingTakeRequest {
            common: CfdStateCommon {
                transition_timestamp: fixed_timestamp,
            },
        };
        let json = serde_json::to_string(&cfd_state).unwrap();
        assert_eq!(
            json,
            r#"{"type":"PendingTakeRequest","payload":{"common":{"transition_timestamp":{"secs_since_epoch":0,"nanos_since_epoch":0}}}}"#
        );

        let cfd_state = CfdState::Accepted {
            common: CfdStateCommon {
                transition_timestamp: fixed_timestamp,
            },
        };
        let json = serde_json::to_string(&cfd_state).unwrap();
        assert_eq!(
            json,
            r#"{"type":"Accepted","payload":{"common":{"transition_timestamp":{"secs_since_epoch":0,"nanos_since_epoch":0}}}}"#
        );

        let cfd_state = CfdState::ContractSetup {
            common: CfdStateCommon {
                transition_timestamp: fixed_timestamp,
            },
        };
        let json = serde_json::to_string(&cfd_state).unwrap();
        assert_eq!(
            json,
            r#"{"type":"ContractSetup","payload":{"common":{"transition_timestamp":{"secs_since_epoch":0,"nanos_since_epoch":0}}}}"#
        );

        let cfd_state = CfdState::Open {
            common: CfdStateCommon {
                transition_timestamp: fixed_timestamp,
            },
            settlement_timestamp: fixed_timestamp,
            margin: Amount::from_btc(0.5).unwrap(),
        };
        let json = serde_json::to_string(&cfd_state).unwrap();
        assert_eq!(
            json,
            r#"{"type":"Open","payload":{"common":{"transition_timestamp":{"secs_since_epoch":0,"nanos_since_epoch":0}},"settlement_timestamp":{"secs_since_epoch":0,"nanos_since_epoch":0},"margin":50000000}}"#
        );

        let cfd_state = CfdState::CloseRequested {
            common: CfdStateCommon {
                transition_timestamp: fixed_timestamp,
            },
        };
        let json = serde_json::to_string(&cfd_state).unwrap();
        assert_eq!(
            json,
            r#"{"type":"CloseRequested","payload":{"common":{"transition_timestamp":{"secs_since_epoch":0,"nanos_since_epoch":0}}}}"#
        );

        let cfd_state = CfdState::PendingClose {
            common: CfdStateCommon {
                transition_timestamp: fixed_timestamp,
            },
        };
        let json = serde_json::to_string(&cfd_state).unwrap();
        assert_eq!(
            json,
            r#"{"type":"PendingClose","payload":{"common":{"transition_timestamp":{"secs_since_epoch":0,"nanos_since_epoch":0}}}}"#
        );

        let cfd_state = CfdState::Closed {
            common: CfdStateCommon {
                transition_timestamp: fixed_timestamp,
            },
        };
        let json = serde_json::to_string(&cfd_state).unwrap();
        assert_eq!(
            json,
            r#"{"type":"Closed","payload":{"common":{"transition_timestamp":{"secs_since_epoch":0,"nanos_since_epoch":0}}}}"#
        );

        let cfd_state = CfdState::Error {
            common: CfdStateCommon {
                transition_timestamp: fixed_timestamp,
            },
        };
        let json = serde_json::to_string(&cfd_state).unwrap();
        assert_eq!(
            json,
            r#"{"type":"Error","payload":{"common":{"transition_timestamp":{"secs_since_epoch":0,"nanos_since_epoch":0}}}}"#
        );
    }

    #[test]
    fn test_secs_into_blocks() {
        let error_margin = f32::EPSILON;

        let duration = Duration::from_secs(600);
        let blocks = duration.as_blocks();
        assert!(blocks - error_margin < 1.0 && blocks + error_margin > 1.0);

        let duration = Duration::from_secs(0);
        let blocks = duration.as_blocks();
        assert!(blocks - error_margin < 0.0 && blocks + error_margin > 0.0);

        let duration = Duration::from_secs(60);
        let blocks = duration.as_blocks();
        assert!(blocks - error_margin < 0.1 && blocks + error_margin > 0.1);
    }
}

/// Contains all data we've assembled about the CFD through the setup protocol.
///
/// All contained signatures are the signatures of THE OTHER PARTY.
/// To use any of these transactions, we need to re-sign them with the correct secret key.
#[derive(Debug)]
pub struct FinalizedCfd {
    pub identity: SecretKey,
    pub revocation: SecretKey,
    pub publish: SecretKey,

    pub lock: PartiallySignedTransaction,
    pub commit: (Transaction, EcdsaAdaptorSignature),
    pub cets: Vec<(
        Transaction,
        EcdsaAdaptorSignature,
        Vec<(Vec<u8>, schnorrsig::PublicKey)>,
    )>,
    pub refund: (Transaction, Signature),
}
