use crate::model::cfd::OrderId;
use crate::model::{Leverage, Position, TradingPair, Usd};
use crate::{bitmex_price_feed, cfd_feed, model};
use bdk::bitcoin::Amount;
use rocket::response::stream::Event;
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct CfdOrder {
    pub id: OrderId,

    pub trading_pair: TradingPair,
    pub position: Position,

    pub price: Usd,

    pub min_quantity: Usd,
    pub max_quantity: Usd,

    pub leverage: Leverage,
    pub liquidation_price: Usd,

    pub creation_timestamp: u64,
    pub term_in_secs: u64,
}

pub trait ToSseEvent {
    fn to_sse_event(&self) -> Event;
}

impl ToSseEvent for Vec<cfd_feed::Cfd> {
    fn to_sse_event(&self) -> Event {
        Event::json(&self).event("cfds")
    }
}

impl ToSseEvent for Option<model::cfd::Order> {
    fn to_sse_event(&self) -> Event {
        let order = self.clone().map(|order| CfdOrder {
            id: order.id,
            trading_pair: order.trading_pair,
            position: order.position,
            price: order.price,
            min_quantity: order.min_quantity,
            max_quantity: order.max_quantity,
            leverage: order.leverage,
            liquidation_price: order.liquidation_price,
            creation_timestamp: order
                .creation_timestamp
                .duration_since(UNIX_EPOCH)
                .expect("timestamp to be convertible to duration since epoch")
                .as_secs(),
            term_in_secs: order.term.as_secs(),
        });

        Event::json(&order).event("order")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WalletInfo {
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    balance: Amount,
    address: String,
    last_updated_at: u64,
}

impl ToSseEvent for model::WalletInfo {
    fn to_sse_event(&self) -> Event {
        let wallet_info = WalletInfo {
            balance: self.balance,
            address: self.address.to_string(),
            last_updated_at: into_unix_secs(self.last_updated_at),
        };

        Event::json(&wallet_info).event("wallet")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Quote {
    bid: Usd,
    ask: Usd,
    last_updated_at: u64,
}

impl ToSseEvent for bitmex_price_feed::Quote {
    fn to_sse_event(&self) -> Event {
        let quote = Quote {
            bid: self.bid,
            ask: self.ask,
            last_updated_at: into_unix_secs(self.timestamp),
        };
        Event::json(&quote).event("quote")
    }
}

/// Convert to the format expected by the frontend
fn into_unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .expect("timestamp to be convertible to duration since epoch")
        .as_secs()
}
