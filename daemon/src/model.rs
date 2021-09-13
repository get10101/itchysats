use std::fmt::{Display, Formatter};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use uuid::Uuid;

pub mod cfd;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
pub struct Usd(pub Decimal);

impl Usd {
    pub const ZERO: Self = Self(Decimal::ZERO);
}

impl Display for Usd {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
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

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TakerId(Uuid);

impl Default for TakerId {
    fn default() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Display for TakerId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
