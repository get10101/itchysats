use std::fmt::{Display, Formatter};

use anyhow::{Context, Result};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use bdk::bitcoin::{Address, Amount};
use std::time::SystemTime;
use uuid::Uuid;

pub mod cfd;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
pub struct Usd(pub Decimal);

impl Usd {
    pub const ZERO: Self = Self(Decimal::ZERO);

    pub fn checked_add(&self, other: Usd) -> Result<Usd> {
        let result = self.0.checked_add(other.0).context("addition error")?;
        Ok(Usd(result))
    }

    pub fn checked_sub(&self, other: Usd) -> Result<Usd> {
        let result = self.0.checked_sub(other.0).context("subtraction error")?;
        Ok(Usd(result))
    }

    pub fn checked_mul(&self, other: Usd) -> Result<Usd> {
        let result = self
            .0
            .checked_mul(other.0)
            .context("multiplication error")?;
        Ok(Usd(result))
    }

    pub fn checked_div(&self, other: Usd) -> Result<Usd> {
        let result = self.0.checked_div(other.0).context("division error")?;
        Ok(Usd(result))
    }

    pub fn try_into_u64(&self) -> Result<u64> {
        self.0.to_u64().context("could not fit decimal into u64")
    }
}

impl Display for Usd {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Decimal> for Usd {
    fn from(decimal: Decimal) -> Self {
        Usd(decimal)
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
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

#[derive(Debug, Clone)]
pub struct WalletInfo {
    pub balance: Amount,
    pub address: Address,
    pub last_updated_at: SystemTime,
}
