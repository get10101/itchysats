use crate::model::Price;
use crate::model::Timestamp;
use crate::supervisor;
use crate::Tasks;
use anyhow::Result;
use async_trait::async_trait;
use futures::SinkExt;
use futures::TryStreamExt;
use rust_decimal::Decimal;
use std::convert::TryFrom;
use std::time::Duration;
use time::OffsetDateTime;
use tokio_tungstenite::tungstenite;
use xtra::Disconnected;
use xtra_productivity::xtra_productivity;

pub const QUOTE_INTERVAL_MINUTES: i64 = 1;

pub struct Actor {
    tasks: Tasks,
    latest_quote: Option<Quote>,
    supervisor: xtra::Address<supervisor::Actor<Self, Error>>,
}

impl Actor {
    pub fn new(supervisor: xtra::Address<supervisor::Actor<Self, Error>>) -> Self {
        Self {
            tasks: Tasks::default(),
            latest_quote: None,
            supervisor,
        }
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");

        self.tasks.add_fallible({
            let this = this.clone();

            async move {
                tracing::debug!("Connecting to BitMex realtime API");

                let (mut connection, _) = tokio_tungstenite::connect_async(format!("wss://www.bitmex.com/realtime?subscribe=quoteBin{QUOTE_INTERVAL_MINUTES}m:XBTUSD")).await.map_err(|e| Error::FailedToConnect { source: e })?;

                tracing::info!("Connected to BitMex realtime API");

                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {
                            tracing::trace!("No message from BitMex in the last 5 seconds, pinging");
                            let _ = connection.send(tungstenite::Message::Ping([0u8; 32].to_vec())).await;
                        },
                        msg = connection.try_next() => {
                            let msg = msg.map_err(|e| Error::Failed { source: e })?;
                            let msg = msg.ok_or(Error::StreamEnded)?;

                            match msg {
                                tungstenite::Message::Pong(_) => {
                                    tracing::trace!("Received pong");
                                    continue;
                                }
                                tungstenite::Message::Text(text) => {
                                    let quote = Quote::from_str(&text).map_err(|e| Error::FailedToParseQuote { source: e })?;

                                    match quote {
                                        Some(quote) => {
                                            tracing::debug!("Received new quote: {:?}", quote);
                                            let is_our_address_disconnected = this.send(NewQuoteReceived(quote)).await.is_err();

                                            // Our task should already be dead and the actor restarted if this happens.
                                            if is_our_address_disconnected {
                                                return Ok(());
                                            }
                                        }
                                        None => {
                                            continue;
                                        }
                                    }
                                }
                                other => {
                                    tracing::trace!("Unsupported message: {:?}", other);
                                    continue;
                                }
                            }
                        },
                    }
                }
            }
        }, |e: Error| async move {
            let _: Result<(), Disconnected> = this.send(e).await;
        });
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: Error, ctx: &mut xtra::Context<Self>) {
        let _ = self
            .supervisor
            .send(supervisor::Stopped { reason: msg })
            .await;
        ctx.stop();
    }

    async fn handle(&mut self, msg: NewQuoteReceived) {
        self.latest_quote = Some(msg.0);
    }

    async fn handle(&mut self, _: LatestQuote) -> Option<Quote> {
        self.latest_quote
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Connection to BitMex API failed")]
    Failed { source: tungstenite::Error },
    #[error("Failed to connect to BitMex API")]
    FailedToConnect { source: tungstenite::Error },
    #[error("Websocket stream to BitMex API closed")]
    StreamEnded,
    #[error("Failed to parse quote")]
    FailedToParseQuote { source: anyhow::Error },
}

/// Private message to update our internal state with the latest quote.
#[derive(Debug)]
struct NewQuoteReceived(Quote);

/// Request the latest quote from the price feed.
#[derive(Debug)]
pub struct LatestQuote;

#[derive(Clone, Copy, Debug)]
pub struct Quote {
    pub timestamp: Timestamp,
    pub bid: Price,
    pub ask: Price,
}

impl Quote {
    fn from_str(text: &str) -> Result<Option<Self>> {
        let table_message = match serde_json::from_str::<wire::TableMessage>(text) {
            Ok(table_message) => table_message,
            Err(_) => {
                tracing::trace!(%text, "Not a 'table' message, skipping...");
                return Ok(None);
            }
        };

        let [quote] = table_message.data;

        Ok(Some(Self {
            timestamp: Timestamp::parse_from_rfc3339(&quote.timestamp)?,
            bid: Price::new(Decimal::try_from(quote.bid_price)?)?,
            ask: Price::new(Decimal::try_from(quote.ask_price)?)?,
        }))
    }

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

        self.timestamp.seconds() < required_quote_timestamp
    }
}

mod wire {
    use serde::Deserialize;

    #[derive(Debug, Clone, Deserialize, PartialEq)]
    pub struct TableMessage {
        pub table: String,
        // we always just expect a single quote, hence the use of an array instead of a vec
        pub data: [QuoteData; 1],
    }

    #[derive(Debug, Clone, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub struct QuoteData {
        pub bid_size: u64,
        pub ask_size: u64,
        pub bid_price: f64,
        pub ask_price: f64,
        pub symbol: String,
        pub timestamp: String,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use time::ext::NumericalDuration;

    #[test]
    fn can_deserialize_quote_message() {
        let quote = Quote::from_str(r#"{"table":"quoteBin1m","action":"insert","data":[{"timestamp":"2021-09-21T02:40:00.000Z","symbol":"XBTUSD","bidSize":50200,"bidPrice":42640.5,"askPrice":42641,"askSize":363600}]}"#).unwrap().unwrap();

        assert_eq!(quote.bid, Price::new(dec!(42640.5)).unwrap());
        assert_eq!(quote.ask, Price::new(dec!(42641)).unwrap());
        assert_eq!(quote.timestamp.seconds(), 1632192000)
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

    fn dummy_quote_at(time: OffsetDateTime) -> Quote {
        Quote {
            timestamp: Timestamp::new(time.unix_timestamp()),
            bid: Price::new(dec!(10)).unwrap(),
            ask: Price::new(dec!(10)).unwrap(),
        }
    }
}
