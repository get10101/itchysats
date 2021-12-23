use crate::model;
use crate::model::Identity;
use crate::model::Timestamp;
use crate::projection;
use crate::projection::Cfd;
use crate::projection::CfdAction;
use crate::projection::CfdOrder;
use crate::projection::Quote;
use bdk::bitcoin::Amount;
use rocket::request::FromParam;
use rocket::response::stream::Event;
use serde::Serialize;

impl<'v> FromParam<'v> for CfdAction {
    type Error = serde_plain::Error;

    fn from_param(param: &'v str) -> Result<Self, Self::Error> {
        let action = serde_plain::from_str(param)?;
        Ok(action)
    }
}

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

impl ToSseEvent for projection::ConnectionStatus {
    fn to_sse_event(&self) -> Event {
        Event::json(&self).event("maker_status")
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
}

impl Heartbeat {
    pub fn new() -> Self {
        Self {
            timestamp: Timestamp::now(),
        }
    }
}

impl Default for Heartbeat {
    fn default() -> Self {
        Self::new()
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
        };

        serde_test::assert_ser_tokens(
            &heartbeat,
            &[
                Token::Struct {
                    name: "Heartbeat",
                    len: 1,
                },
                Token::Str("timestamp"),
                Token::NewtypeStruct { name: "Timestamp" },
                Token::I64(0),
                Token::StructEnd,
            ],
        );
    }
}
