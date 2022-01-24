use crate::connection;
use crate::model;
use crate::model::Identity;
use crate::model::Timestamp;
use crate::projection::Cfd;
use crate::projection::CfdOrder;
use crate::projection::Quote;
use crate::to_sse_event::ConnectionCloseReason::MakerVersionOutdated;
use crate::to_sse_event::ConnectionCloseReason::TakerVersionOutdated;
use bdk::bitcoin::Amount;
use rocket::response::stream::Event;
use serde::Serialize;

pub trait ToSseEvent {
    fn to_sse_event(&self) -> Event;
}

impl ToSseEvent for Vec<Cfd> {
    fn to_sse_event(&self) -> Event {
        Event::json(&self).event("cfds")
    }
}

impl ToSseEvent for Vec<Identity> {
    fn to_sse_event(&self) -> Event {
        Event::json(&self).event("takers")
    }
}

impl ToSseEvent for Option<CfdOrder> {
    fn to_sse_event(&self) -> Event {
        Event::json(&self).event("order")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WalletInfo {
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    balance: Amount,
    address: String,
    last_updated_at: Timestamp,
}

impl ToSseEvent for Option<model::WalletInfo> {
    fn to_sse_event(&self) -> Event {
        let wallet_info = self.as_ref().map(|wallet_info| WalletInfo {
            balance: wallet_info.balance,
            address: wallet_info.address.to_string(),
            last_updated_at: wallet_info.last_updated_at,
        });

        Event::json(&wallet_info).event("wallet")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionStatus {
    online: bool,
    connection_close_reason: Option<ConnectionCloseReason>,
}

#[derive(Debug, Clone, Serialize)]
pub enum ConnectionCloseReason {
    MakerVersionOutdated,
    TakerVersionOutdated,
}

impl ToSseEvent for connection::ConnectionStatus {
    fn to_sse_event(&self) -> Event {
        let connected = match self {
            connection::ConnectionStatus::Online => ConnectionStatus {
                online: true,
                connection_close_reason: None,
            },
            connection::ConnectionStatus::Offline { reason } => ConnectionStatus {
                online: false,
                connection_close_reason: reason.as_ref().map(|g| match g {
                    connection::ConnectionCloseReason::VersionMismatch {
                        maker_version,
                        taker_version,
                    } => {
                        if *maker_version < *taker_version {
                            MakerVersionOutdated
                        } else {
                            TakerVersionOutdated
                        }
                    }
                }),
            },
        };

        Event::json(&connected).event("maker_status")
    }
}

impl ToSseEvent for Option<Quote> {
    fn to_sse_event(&self) -> Event {
        Event::json(self).event("quote")
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Heartbeat {
    timestamp: Timestamp,
    interval: u64,
}

impl Heartbeat {
    pub fn new(interval: u64) -> Self {
        Self {
            timestamp: Timestamp::now(),
            interval,
        }
    }
}

impl ToSseEvent for Heartbeat {
    fn to_sse_event(&self) -> Event {
        Event::json(self).event("heartbeat")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_test::Token;

    #[test]
    fn heartbeat_serialization() {
        let heartbeat = Heartbeat {
            timestamp: Timestamp::new(0),
            interval: 1,
        };

        serde_test::assert_ser_tokens(
            &heartbeat,
            &[
                Token::Struct {
                    name: "Heartbeat",
                    len: 2,
                },
                Token::Str("timestamp"),
                Token::NewtypeStruct { name: "Timestamp" },
                Token::I64(0),
                Token::Str("interval"),
                Token::U64(1),
                Token::StructEnd,
            ],
        );
    }
}
