use crate::model::Usd;
use anyhow::{Context, Result};
use futures::{StreamExt, TryStreamExt};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::convert::TryFrom;
use std::future::Future;
use std::time::SystemTime;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite;

/// Connects to the BitMex price feed, returning the polling task and a watch channel that will
/// always hold the last quote.
pub async fn new() -> Result<(impl Future<Output = ()>, watch::Receiver<Quote>)> {
    let (connection, _) = tokio_tungstenite::connect_async(
        "wss://www.bitmex.com/realtime?subscribe=quoteBin1m:XBTUSD",
    )
    .await?;
    let mut quotes = connection
        .inspect_err(|e| tracing::warn!("Error on websocket stream: {}", e))
        .map(|msg| Quote::from_message(msg?))
        .filter_map(|result| async move { result.transpose() })
        .boxed()
        .fuse();

    tracing::info!("Connected to BitMex realtime API");

    let first_quote = quotes.select_next_some().await?;
    let (sender, receiver) = watch::channel(first_quote);

    let task = async move {
        while let Ok(Some(quote)) = quotes.try_next().await {
            if sender.send(quote).is_err() {
                break; // If the receiver dies, we can exit the loop.
            }
        }

        tracing::warn!("Failed to read quote from websocket");
    };

    Ok((task, receiver))
}

#[derive(Clone, Debug)]
pub struct Quote {
    pub timestamp: SystemTime,
    pub bid: Usd,
    pub ask: Usd,
}

impl Quote {
    fn from_message(message: tungstenite::Message) -> Result<Option<Self>> {
        let text_message = match message {
            tungstenite::Message::Text(text_message) => text_message,
            _ => anyhow::bail!("Bad message type, only text is supported"),
        };

        let table_message = match serde_json::from_str::<wire::TableMessage>(&text_message) {
            Ok(table_message) => table_message,
            Err(_) => return Ok(None),
        };

        let [quote] = table_message.data;

        Ok(Some(Self {
            timestamp: SystemTime::now(),
            bid: Usd::from(Decimal::try_from(quote.bid_price)?),
            ask: Usd::from(Decimal::try_from(quote.ask_price)?),
        }))
    }

    #[allow(dead_code)] // Not used by all binaries.
    pub fn for_maker(&self) -> Usd {
        self.ask
    }

    #[allow(dead_code)] // Not used by all binaries.
    pub fn for_taker(&self) -> Usd {
        // TODO: Verify whether this is correct
        self.mid_range().unwrap()
    }

    fn mid_range(&self) -> Result<Usd> {
        Ok(Usd((self.bid.checked_add(self.ask))?
            .0
            .checked_div(dec!(2))
            .context("division error")?))
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn can_deserialize_quote_message() {
        let message = tungstenite::Message::Text(r#"{"table":"quoteBin1m","action":"insert","data":[{"timestamp":"2021-09-21T02:40:00.000Z","symbol":"XBTUSD","bidSize":50200,"bidPrice":42640.5,"askPrice":42641,"askSize":363600}]}"#.to_owned());

        let quote = Quote::from_message(message).unwrap().unwrap();

        assert_eq!(quote.bid, Usd(dec!(42640.5)));
        assert_eq!(quote.ask, Usd(dec!(42641)));
    }
}
