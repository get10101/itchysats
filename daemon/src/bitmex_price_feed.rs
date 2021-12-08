use crate::model::{Price, Timestamp};
use crate::{projection, Tasks};
use anyhow::Result;
use futures::{SinkExt, TryStreamExt};
use rust_decimal::Decimal;
use std::convert::TryFrom;
use std::time::Duration;
use tokio_tungstenite::tungstenite;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;

const URL: &str = "wss://www.bitmex.com/realtime?subscribe=quoteBin1m:XBTUSD";

pub struct Actor {
    tasks: Tasks,
    receiver: Box<dyn MessageChannel<projection::Update<Quote>>>,
}

impl Actor {
    pub fn new(receiver: impl MessageChannel<projection::Update<Quote>> + 'static) -> Self {
        Self {
            tasks: Tasks::default(),
            receiver: Box::new(receiver),
        }
    }
}

impl xtra::Actor for Actor {}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: NotifyNoConnection, ctx: &mut xtra::Context<Self>) {
        match msg {
            NotifyNoConnection::Failed { error } => {
                tracing::warn!("Connection to BitMex realtime API failed: {}", error)
            }
            NotifyNoConnection::StreamEnded => {
                tracing::warn!("Connection to BitMex realtime API closed")
            }
        }

        let this = ctx.address().expect("we are alive");

        self.tasks.add(connect_until_successful(this));
    }

    async fn handle(&mut self, _: Connect, ctx: &mut xtra::Context<Self>) -> Result<()> {
        tracing::debug!("Connecting to BitMex realtime API");

        let (mut connection, _) = tokio_tungstenite::connect_async(URL).await?;

        tracing::info!("Connected to BitMex realtime API");

        let this = ctx.address().expect("we are alive");

        self.tasks.add({
            let receiver = self.receiver.clone_channel();
            async move {
                let no_connection = loop {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {
                            tracing::trace!("No message from BitMex in the last 5 seconds, pinging");
                            let _ = connection.send(tungstenite::Message::Ping([0u8; 32].to_vec())).await;
                        },
                        msg = connection.try_next() => {
                            match msg {
                                Ok(Some(tungstenite::Message::Pong(_))) => {
                                    tracing::trace!("Received pong");
                                    continue;
                                }
                                Ok(Some(tungstenite::Message::Text(text))) => {
                                    match Quote::from_str(&text) {
                                        Ok(None) => {
                                            continue;
                                        }
                                        Ok(Some(quote)) => {
                                            if receiver.send(projection::Update(quote)).await.is_err() {
                                                return; // if the receiver dies, our job is done
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!("Failed to parse quote: {:#}", e);
                                            return;
                                        }
                                    }
                                }
                                Ok(Some(other)) => {
                                    tracing::trace!("Unsupported message: {:?}", other);
                                    continue;
                                }
                                Ok(None) => {
                                    break NotifyNoConnection::StreamEnded
                                }
                                Err(e) => {
                                    break NotifyNoConnection::Failed { error: e }
                                }
                            }
                        },
                    }
                };

                let _ = this.send(no_connection).await;
            }
        });

        Ok(())
    }
}

async fn connect_until_successful(this: xtra::Address<Actor>) {
    while let Err(e) = this
        .send(Connect)
        .await
        .expect("always connected to ourselves")
    {
        tracing::warn!("Failed to connect to BitMex realtime API: {:#}", e);
    }
}

pub struct Connect;

enum NotifyNoConnection {
    Failed { error: tungstenite::Error },
    StreamEnded,
}

#[derive(Clone, Debug)]
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
        let quote = Quote::from_str(r#"{"table":"quoteBin1m","action":"insert","data":[{"timestamp":"2021-09-21T02:40:00.000Z","symbol":"XBTUSD","bidSize":50200,"bidPrice":42640.5,"askPrice":42641,"askSize":363600}]}"#).unwrap().unwrap();

        assert_eq!(quote.bid, Price::new(dec!(42640.5)).unwrap());
        assert_eq!(quote.ask, Price::new(dec!(42641)).unwrap());
        assert_eq!(quote.timestamp.seconds(), 1632192000)
    }
}
