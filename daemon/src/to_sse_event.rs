use crate::model;
use crate::model::cfd::OrderId;
use crate::model::{Leverage, Position, TradingPair, Usd};
use bdk::bitcoin::Amount;
use rocket::response::stream::Event;
use serde::Serialize;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Serialize)]
pub struct Cfd {
    pub order_id: OrderId,
    pub initial_price: Usd,

    pub leverage: Leverage,
    pub trading_pair: TradingPair,
    pub position: Position,
    pub liquidation_price: Usd,

    pub quantity_usd: Usd,

    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub margin: Amount,

    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub profit_btc: Amount,
    pub profit_usd: Usd,

    pub state: String,
    pub state_transition_timestamp: u64,
}

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

impl ToSseEvent for Vec<model::cfd::Cfd> {
    // TODO: This conversion can fail, we might want to change the API
    fn to_sse_event(&self) -> Event {
        let cfds = self
            .iter()
            .map(|cfd| {
                // TODO: Get the actual current price here
                let current_price = Usd::ZERO;
                let (profit_btc, profit_usd) = cfd.profit(current_price).unwrap();

                Cfd {
                    order_id: cfd.order.id,
                    initial_price: cfd.order.price,
                    leverage: cfd.order.leverage,
                    trading_pair: cfd.order.trading_pair.clone(),
                    position: cfd.position(),
                    liquidation_price: cfd.order.liquidation_price,
                    quantity_usd: cfd.quantity_usd,
                    profit_btc,
                    profit_usd,
                    state: cfd.state.to_string(),
                    state_transition_timestamp: cfd
                        .state
                        .get_transition_timestamp()
                        .duration_since(UNIX_EPOCH)
                        .expect("timestamp to be convertable to duration since epoch")
                        .as_secs(),

                    // TODO: Depending on the state the margin might be set (i.e. in Open we save it
                    // in the DB internally) and does not have to be calculated
                    margin: cfd.margin().unwrap(),
                }
            })
            .collect::<Vec<Cfd>>();

        Event::json(&cfds).event("cfds")
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
            last_updated_at: self
                .last_updated_at
                .duration_since(UNIX_EPOCH)
                .expect("timestamp to be convertible to duration since epoch")
                .as_secs(),
        };

        Event::json(&wallet_info).event("wallet")
    }
}
