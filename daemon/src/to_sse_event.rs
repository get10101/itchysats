use crate::connection::ConnectionStatus;
use crate::model;
use crate::model::Timestamp;
use crate::projection::{Cfd, CfdAction, CfdOrder, CfdsWithAuxData, Identity, Quote};
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

impl ToSseEvent for CfdsWithAuxData {
    // TODO: This conversion can fail, we might want to change the API
    fn to_sse_event(&self) -> Event {
        let cfds: Vec<Cfd> = self.into();
        Event::json(&cfds).event("cfds")
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

impl ToSseEvent for model::WalletInfo {
    fn to_sse_event(&self) -> Event {
        let wallet_info = WalletInfo {
            balance: self.balance,
            address: self.address.to_string(),
            last_updated_at: self.last_updated_at,
        };

        Event::json(&wallet_info).event("wallet")
    }
}

impl ToSseEvent for ConnectionStatus {
    fn to_sse_event(&self) -> Event {
        let connected = match self {
            ConnectionStatus::Online => true,
            ConnectionStatus::Offline => false,
        };

        Event::json(&connected).event("maker_status")
    }
}

impl ToSseEvent for Quote {
    fn to_sse_event(&self) -> Event {
        Event::json(self).event("quote")
    }
}
