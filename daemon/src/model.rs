use crate::olivia;
use anyhow::{Context, Result};
use bdk::bitcoin::{Address, Amount};
use reqwest::Url;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_with::{DeserializeFromStr, SerializeDisplay};
use std::time::SystemTime;
use std::{fmt, str};
use time::{OffsetDateTime, PrimitiveDateTime, Time};
use uuid::Uuid;

pub mod cfd;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, PartialOrd)]
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

#[derive(
    Debug, Clone, Copy, SerializeDisplay, DeserializeFromStr, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
pub struct BitMexPriceEventId {
    /// The timestamp this price event refers to.
    timestamp: OffsetDateTime,
    digits: usize,
}

impl BitMexPriceEventId {
    pub fn new(timestamp: OffsetDateTime, digits: usize) -> Self {
        let (hours, minutes, seconds) = timestamp.time().as_hms();
        let time_without_nanos =
            Time::from_hms(hours, minutes, seconds).expect("original timestamp was valid");

        let timestamp_without_nanos = timestamp.replace_time(time_without_nanos);

        Self {
            timestamp: timestamp_without_nanos,
            digits,
        }
    }

    pub fn with_20_digits(timestamp: OffsetDateTime) -> Self {
        Self::new(timestamp, 20)
    }

    /// Checks whether this event has likely already occurred.
    ///
    /// We can't be sure about it because our local clock might be off from the oracle's clock.
    pub fn has_likely_occured(&self) -> bool {
        let now = OffsetDateTime::now_utc();

        now > self.timestamp
    }

    pub fn to_olivia_url(self) -> Url {
        "https://h00.ooo"
            .parse::<Url>()
            .expect("valid URL from constant")
            .join(&self.to_string())
            .expect("Event id can be joined")
    }
}

impl fmt::Display for BitMexPriceEventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "/x/BitMEX/BXBT/{}.price?n={}",
            self.timestamp
                .format(&olivia::EVENT_TIME_FORMAT)
                .expect("should always format and we can't return an error here"),
            self.digits
        )
    }
}

impl str::FromStr for BitMexPriceEventId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let remaining = s.trim_start_matches("/x/BitMEX/BXBT/");
        let (timestamp, rest) = remaining.split_at(19);
        let digits = rest.trim_start_matches(".price?n=");

        Ok(Self {
            timestamp: PrimitiveDateTime::parse(timestamp, &olivia::EVENT_TIME_FORMAT)
                .with_context(|| format!("Failed to parse {} as timestamp", timestamp))?
                .assume_utc(),
            digits: digits.parse()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn to_olivia_url() {
        let url = BitMexPriceEventId::with_20_digits(datetime!(2021-09-23 10:00:00).assume_utc())
            .to_olivia_url();

        assert_eq!(
            url,
            "https://h00.ooo/x/BitMEX/BXBT/2021-09-23T10:00:00.price?n=20"
                .parse()
                .unwrap()
        );
    }

    #[test]
    fn parse_event_id() {
        let parsed = "/x/BitMEX/BXBT/2021-09-23T10:00:00.price?n=20"
            .parse::<BitMexPriceEventId>()
            .unwrap();
        let expected =
            BitMexPriceEventId::with_20_digits(datetime!(2021-09-23 10:00:00).assume_utc());

        assert_eq!(parsed, expected);
    }

    #[test]
    fn new_event_has_no_nanos() {
        let now = BitMexPriceEventId::with_20_digits(OffsetDateTime::now_utc());

        assert_eq!(now.timestamp.nanosecond(), 0);
    }

    #[test]
    fn has_occured_if_in_the_past() {
        let past_event =
            BitMexPriceEventId::with_20_digits(datetime!(2021-09-23 10:00:00).assume_utc());

        assert!(past_event.has_likely_occured());
    }
}
