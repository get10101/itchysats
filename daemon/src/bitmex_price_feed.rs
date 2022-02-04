use crate::model::Price;
use anyhow::bail;
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;
use std::time::Duration;
use time::ext::NumericalDuration;
use time::OffsetDateTime;
use tokio_tasks::Tasks;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

pub const QUOTE_INTERVAL_MINUTES: u64 = 1;
const QUOTE_SYNC_INTERVAL_SECONDS: u64 = (QUOTE_INTERVAL_MINUTES * 60) - 15;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct Quote {
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    #[serde(rename = "bidPrice")]
    pub bid: Price,
    #[serde(rename = "askPrice")]
    pub ask: Price,
}

impl Quote {
    pub fn for_maker(&self) -> Price {
        self.ask
    }

    pub fn for_taker(&self) -> Price {
        self.mid_range()
    }

    fn mid_range(&self) -> Price {
        (self.bid + self.ask) / 2
    }

    pub fn is_older_than(&self, duration: time::Duration) -> bool {
        let required_quote_timestamp = (OffsetDateTime::now_utc() - duration).unix_timestamp();

        self.timestamp.unix_timestamp() < required_quote_timestamp
    }
}

#[derive(Default)]
pub struct Actor {
    client: Client,
    latest_quote: Option<Quote>,
    tasks: Tasks,
}

impl Actor {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
            latest_quote: None,
            tasks: Tasks::default(),
        }
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");
        self.tasks
            .add(this.send_interval(Duration::from_secs(QUOTE_SYNC_INTERVAL_SECONDS), || Sync));
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: Sync) {
        tracing::trace!("Syncing latest XBTUSD quote from BitMEX");

        if let Some(quote) = &self.latest_quote {
            if !quote.is_older_than(1.minutes()) {
                tracing::trace!("Aborting sync: too early");
                return;
            }
        }

        match self.client.get_latest_quote().await {
            Ok(quote) => {
                self.latest_quote = Some(quote);
                tracing::debug!("Updated XBTUSD BitMEX latest quote: {quote:?}");
            }

            Err(e) => {
                tracing::warn!("Could not fetch latest XBTUSD BitMEX quote: {e:#?}")
            }
        }
    }

    async fn handle(&mut self, _: LatestQuote) -> Option<Quote> {
        self.latest_quote
    }
}

/// Trigger a request for the latest XBTUSD BitMEX quote.
struct Sync;

/// Request the latest XBTUSD quote retrieved from BitMEX.
#[derive(Debug)]
pub struct LatestQuote;

const BITMEX_QUOTE_URL: &str = "https://www.bitmex.com/api/v1/quote/bucketed";

#[derive(Clone, Debug, Serialize)]
/// Get previous quotes in time buckets.
struct GetQuoteBucketedRequest {
    /// Time interval to bucket by. Available options: [1m,5m,1h,1d].
    #[serde(rename = "binSize")]
    pub bin_size: BinSize,
    /// Instrument symbol.
    pub symbol: String,
    /// Number of results to fetch.
    pub count: u32,
    /// If true, will sort results newest first.
    pub reverse: bool,
}

impl GetQuoteBucketedRequest {
    fn latest_xbt_usd() -> Self {
        Self {
            bin_size: BinSize::M1,
            symbol: "XBTUSD".to_string(),
            count: 1,
            reverse: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename = "XBTUSD")]
struct XbtUsd;

#[derive(Clone, Debug, Serialize)]
enum BinSize {
    #[serde(rename = "1m")]
    M1,
}

// The error response from bitmex;
#[derive(Deserialize, Debug, Clone)]
struct BitmexErrorResponse {
    error: BitmexErrorMessage,
}

#[derive(Deserialize, Debug, Clone)]
struct BitmexErrorMessage {
    message: String,
    name: String,
}

#[derive(Default)]
struct Client(reqwest::Client);

impl Client {
    fn new() -> Self {
        Self(reqwest::Client::new())
    }

    async fn get_latest_quote(&self) -> Result<Quote> {
        let mut url = reqwest::Url::parse(BITMEX_QUOTE_URL).expect("valid URL");
        let query = serde_urlencoded::to_string(GetQuoteBucketedRequest::latest_xbt_usd())
            .expect("valid query");
        url.set_query(Some(&query));

        let resp = self
            .0
            .request(reqwest::Method::GET, url)
            .header("content-type", "application/json")
            .body("")
            .send()
            .await?;

        let status = resp.status();
        let content = resp.text().await?;
        if !status.is_success() {
            match serde_json::from_str::<BitmexErrorResponse>(&content) {
                Ok(BitmexErrorResponse {
                    error: BitmexErrorMessage { message, name },
                }) => {
                    bail!("Failed to fetch latest quote from BitMEX: {name}, {message}")
                }
                Err(deser_err) => {
                    tracing::warn!("Failed to deserialize BitMEX error: {}", deser_err);
                    bail!("Failed to fetch latest quote from BitMEX: {}", content)
                }
            }
        }

        let quotes = match serde_json::from_str::<Vec<Quote>>(&content) {
            Ok(quotes) => quotes,
            Err(err) => bail!(
                "Failed to deserialize BitMEX latest quotes {} from BitMEX: {}",
                content,
                err
            ),
        };

        let quote = match quotes.as_slice() {
            [quote] => *quote,
            _ => bail!("Wrong number of quotes received from BitMEX"),
        };

        Ok(quote)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use time::macros::datetime;

    #[tokio::test]
    async fn can_deserialize_quote_response() {
        let quote_response = "[{\"timestamp\":\"2022-02-04T01:58:00.000Z\",\"symbol\":\"XBTUSD\",\"bidSize\":460500,\"bidPrice\":37197.5,\"askPrice\":37198,\"askSize\":328600}]";

        let quotes = serde_json::from_str::<Vec<Quote>>(quote_response).unwrap();

        assert_eq!(
            quotes,
            vec![Quote {
                timestamp: datetime!(2022-02-04 01:58:00).assume_utc(),
                bid: Price::new(dec!(37197.5)).unwrap(),
                ask: Price::new(dec!(37198)).unwrap(),
            }]
        )
    }

    #[test]
    fn quote_from_now_is_not_old() {
        let quote = dummy_quote_at(OffsetDateTime::now_utc());

        let is_older = quote.is_older_than(1.minutes());

        assert!(!is_older)
    }

    #[test]
    fn quote_from_one_hour_ago_is_old() {
        let quote = dummy_quote_at(OffsetDateTime::now_utc() - 1.hours());

        let is_older = quote.is_older_than(1.minutes());

        assert!(is_older)
    }

    fn dummy_quote_at(timestamp: OffsetDateTime) -> Quote {
        Quote {
            timestamp,
            bid: Price::new(dec!(10)).unwrap(),
            ask: Price::new(dec!(10)).unwrap(),
        }
    }
}
