use async_stream::stream;
use futures::SinkExt;
use futures::Stream;
use futures::StreamExt;
use std::time::Duration;
use tokio_tungstenite::tungstenite;

pub use tokio_tungstenite::tungstenite::Error;

/// Connects to the BitMex websocket API, subscribes to the specified topics (comma-separated) and
/// yields all messages.
///
/// To keep the connection alive, a websocket `Ping` will be sent every 5 seconds in case no other
/// message was received in-between. This is according to BitMex's API documentation: https://www.bitmex.com/app/wsAPI#Heartbeats
pub fn subscribe<const N: usize>(
    topics: [String; N],
) -> impl Stream<Item = Result<String, Error>> + Unpin {
    let stream = stream! {
        tracing::debug!("Connecting to BitMex realtime API");

        let subscription = topics.join(",");
        let (mut connection, _) = tokio_tungstenite::connect_async(format!("wss://www.bitmex.com/realtime?subscribe={subscription}")).await?;

        tracing::info!("Connected to BitMex realtime API");

        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    tracing::trace!("No message from BitMex in the last 5 seconds, pinging");
                    let _ = connection.send(tungstenite::Message::Ping([0u8; 32].to_vec())).await;
                },
                msg = connection.next() => {
                    let msg = match msg {
                        Some(Ok(msg)) => {
                            msg
                        },
                        None => {
                            return;
                        }
                        Some(Err(e)) => {
                            yield Err(e);
                            return;
                        }
                    };

                    match msg {
                        tungstenite::Message::Pong(_) => {
                            tracing::trace!("Received pong");
                            continue;
                        }
                        tungstenite::Message::Text(text) => {
                            yield Ok(text);
                        }
                        other => {
                            tracing::trace!("Unsupported message: {:?}", other);
                            continue;
                        }
                    }
                }
            }
        }
    };

    stream.boxed()
}
