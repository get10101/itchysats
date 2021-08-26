use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

pub mod cfd;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
pub struct Usd(pub Decimal);

impl Usd {
    pub const ZERO: Self = Self(Decimal::ZERO);
}

impl Usd {
    pub fn to_sat_precision(&self) -> Self {
        Self(self.0.round_dp(8))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Leverage(pub u8);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TradingPair {
    BtcUsd,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Position {
    Buy,
    Sell,
}
