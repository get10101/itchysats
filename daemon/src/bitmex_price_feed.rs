use crate::model::{Price, Timestamp};
use crate::projection;
use anyhow::Result;
use futures::{StreamExt, TryStreamExt};
use rust_decimal::Decimal;
use std::convert::TryFrom;
use std::future::Future;
use tokio_tungstenite::tungstenite;
use xtra::prelude::MessageChannel;

/// Connects to the BitMex price feed, returning the polling task and a watch channel that will
/// always hold the last quote.
pub async fn new(
    msg_channel: impl MessageChannel<projection::Update<Quote>>,
) -> Result<(impl Future<Output = ()>, Quote)> {
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

    let task = async move {
        while let Ok(Some(quote)) = quotes.try_next().await {
            if msg_channel.send(projection::Update(quote)).await.is_err() {
                break; // If the receiver dies, we can exit the loop.
            }
        }

        tracing::warn!("Failed to read quote from websocket");
    };

    Ok((task, first_quote))
}

#[derive(Clone, Debug)]
pub struct Quote {
    pub timestamp: Timestamp,
    pub bid: Price,
    pub ask: Price,
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
            timestamp: Timestamp::parse_from_rfc3339(&quote.timestamp)?,
            bid: Price::new(Decimal::try_from(quote.bid_price)?)?,
            ask: Price::new(Decimal::try_from(quote.ask_price)?)?,
        }))
    }

    pub fn for_maker(&self) -> Price {
        self.ask
    }

    pub fn for_taker(&self) -> Price {
        // TODO: verify whether this is correct
        self.mid_range()
    }

    fn mid_range(&self) -> Price {
        (self.bid + self.ask) / 2
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

    #[test]
    fn can_deserialize_quote_message() {
        let message = tungstenite::Message::Text(r#"{"table":"quoteBin1m","action":"insert","data":[{"timestamp":"2021-09-21T02:40:00.000Z","symbol":"XBTUSD","bidSize":50200,"bidPrice":42640.5,"askPrice":42641,"askSize":363600}]}"#.to_owned());

        let quote = Quote::from_message(message).unwrap().unwrap();

        assert_eq!(quote.bid, Price::new(dec!(42640.5)).unwrap());
        assert_eq!(quote.ask, Price::new(dec!(42641)).unwrap());
        assert_eq!(quote.timestamp.seconds(), 1632192000)
    }
}
