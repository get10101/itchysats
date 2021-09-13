use crate::model::{Leverage, Position, TradingPair, Usd};
use anyhow::{Context, Result};
use bdk::bitcoin::Amount;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
pub struct CfdOfferId(Uuid);

impl Default for CfdOfferId {
    fn default() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Display for CfdOfferId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A concrete offer created by a maker for a taker
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CfdOffer {
    pub id: CfdOfferId,

    pub trading_pair: TradingPair,
    pub position: Position,

    pub price: Usd,

    // TODO: [post-MVP] Representation of the contract size; at the moment the contract size is always 1 USD
    pub min_quantity: Usd,
    pub max_quantity: Usd,

    // TODO: [post-MVP] Allow different values
    pub leverage: Leverage,
    pub liquidation_price: Usd,

    pub creation_timestamp: SystemTime,

    /// The duration that will be used for calculating the settlement timestamp
    pub term: Duration,
}

#[allow(dead_code)] // Only one binary and the tests use this.
impl CfdOffer {
    pub fn from_default_with_price(price: Usd) -> Result<Self> {
        let leverage = Leverage(5);
        let maintenance_margin_rate = dec!(0.005);
        let liquidation_price =
            calculate_liquidation_price(&leverage, &price, &maintenance_margin_rate)?;

        Ok(CfdOffer {
            id: CfdOfferId::default(),
            price,
            min_quantity: Usd(dec!(1000)),
            max_quantity: Usd(dec!(10000)),
            leverage,
            trading_pair: TradingPair::BtcUsd,
            liquidation_price,
            position: Position::Sell,
            creation_timestamp: SystemTime::now(),
            term: Duration::from_secs(60 * 60 * 8), // 8 hours
        })
    }
    pub fn with_min_quantity(mut self, min_quantity: Usd) -> CfdOffer {
        self.min_quantity = min_quantity;
        self
    }

    pub fn with_max_quantity(mut self, max_quantity: Usd) -> CfdOffer {
        self.max_quantity = max_quantity;
        self
    }
}

fn calculate_liquidation_price(
    leverage: &Leverage,
    price: &Usd,
    maintenance_margin_rate: &Decimal,
) -> Result<Usd> {
    let leverage = Decimal::from(leverage.0);
    let price = price.0;

    // liquidation price calc in isolated margin mode
    // currently based on: https://help.bybit.com/hc/en-us/articles/360039261334-How-to-calculate-Liquidation-Price-Inverse-Contract-
    let liquidation_price = price
        .checked_mul(leverage)
        .context("multiplication error")?
        .checked_div(
            leverage
                .checked_add(Decimal::ONE)
                .context("addition error")?
                .checked_sub(
                    maintenance_margin_rate
                        .checked_mul(leverage)
                        .context("multiplication error")?,
                )
                .context("subtraction error")?,
        )
        .context("division error")?;

    Ok(Usd(liquidation_price))
}

/// The taker POSTs this to create a Cfd
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CfdTakeRequest {
    pub offer_id: CfdOfferId,
    pub quantity: Usd,
}

/// The maker POSTs this to create a new CfdOffer
// TODO: Use Rocket form?
#[derive(Debug, Clone, Deserialize)]
pub struct CfdNewOfferRequest {
    pub price: Usd,
    // TODO: [post-MVP] Representation of the contract size; at the moment the contract size is always 1 USD
    pub min_quantity: Usd,
    pub max_quantity: Usd,
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
    },

    /// Requested close the position, but we have not passed that on to the blockchain yet.
    ///
    /// This state applies to taker and maker.
    CloseRequested { common: CfdStateCommon },
    /// The close transaction (CET) was published on the Bitcoin blockchain but we don't have a confirmation yet.
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
    // fn get_common(&self) -> CfdStateCommon {
    //     let common = match self {
    //         CfdState::TakeRequested { common } => common,
    //         CfdState::PendingTakeRequest { common } => common,
    //         CfdState::Accepted { common } => common,
    //         CfdState::Rejected { common } => common,
    //         CfdState::ContractSetup { common } => common,
    //         CfdState::Open { common, .. } => common,
    //         CfdState::CloseRequested { common } => common,
    //         CfdState::PendingClose { common } => common,
    //         CfdState::Closed { common } => common,
    //         CfdState::Error { common } => common,
    //     };

    //     *common
    // }

    // pub fn get_transition_timestamp(&self) -> SystemTime {
    //     self.get_common().transition_timestamp
    // }
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
    pub offer_id: CfdOfferId,
    pub initial_price: Usd,

    pub leverage: Leverage,
    pub trading_pair: TradingPair,
    pub position: Position,
    pub liquidation_price: Usd,

    pub quantity_usd: Usd,

    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub profit_btc: Amount,
    pub profit_usd: Usd,

    pub state: CfdState,
}

impl Cfd {
    pub fn new(
        cfd_offer: CfdOffer,
        quantity: Usd,
        state: CfdState,
        current_price: Usd,
    ) -> Result<Self> {
        let (profit_btc, profit_usd) =
            calculate_profit(cfd_offer.price, current_price, dec!(0.005), Usd(dec!(0.1)))?;

        Ok(Cfd {
            offer_id: cfd_offer.id,
            initial_price: cfd_offer.price,
            leverage: cfd_offer.leverage,
            trading_pair: cfd_offer.trading_pair,
            position: cfd_offer.position,
            liquidation_price: cfd_offer.liquidation_price,
            quantity_usd: quantity,
            // initially the profit is zero
            profit_btc,
            profit_usd,
            state,
        })
    }
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
    fn serialize_cfd_state_snapshot() {
        // This test is to prevent us from breaking the cfd_state API used by the UI and database!
        // We serialize the state into the database, so changes to the enum result in breaking program version changes.

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
        };
        let json = serde_json::to_string(&cfd_state).unwrap();
        assert_eq!(
            json,
            r#"{"type":"Open","payload":{"common":{"transition_timestamp":{"secs_since_epoch":0,"nanos_since_epoch":0}},"settlement_timestamp":{"secs_since_epoch":0,"nanos_since_epoch":0}}}"#
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
}
