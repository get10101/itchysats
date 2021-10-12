use anyhow::{Context, Result};
use bdk::bitcoin::{Address, Amount};
use reqwest::Url;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use std::time::SystemTime;
use uuid::Uuid;

pub mod cfd;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
pub struct Usd(pub Decimal);

impl Usd {
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

impl fmt::Display for Usd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Decimal> for Usd {
    fn from(decimal: Decimal) -> Self {
        Usd(decimal)
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
pub struct Percent(pub Decimal);

impl fmt::Display for Percent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.round_dp(2).fmt(f)
    }
}

impl From<Decimal> for Percent {
    fn from(decimal: Decimal) -> Self {
        Percent(decimal)
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

impl fmt::Display for TakerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone)]
pub struct WalletInfo {
    pub balance: Amount,
    pub address: Address,
    pub last_updated_at: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BitMexPriceEventId(pub String);

impl BitMexPriceEventId {
    pub fn to_olivia_url(&self) -> Url {
        Url::from_str("https://h00.ooo")
            .expect("valid URL from constant")
            .join(self.0.as_str())
            .expect("Event id can be joined")
    }
}

impl fmt::Display for BitMexPriceEventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_olivia_url() {
        let url = BitMexPriceEventId("/x/BitMEX/BXBT/2021-09-23T10:00:00.price?n=20".to_string())
            .to_olivia_url();

        assert_eq!(
            url,
            Url::from_str("https://h00.ooo/x/BitMEX/BXBT/2021-09-23T10:00:00.price?n=20").unwrap()
        );
    }
}
