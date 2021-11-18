use crate::dto::{to_dto_cfd, Cfd, CfdOrder, CfdsWithAuxData, Quote, WalletInfo};
use crate::{bitmex_price_feed, model};
use rocket::response::stream::Event;
use std::convert::TryInto;

pub trait ToSseEvent {
    fn to_sse_event(&self) -> Event;
}

impl ToSseEvent for CfdsWithAuxData {
    // TODO: This conversion can fail, we might want to change the API
    fn to_sse_event(&self) -> Event {
        let current_price = self.current_price;
        let network = self.network;

        let cfds = self
            .cfds
            .iter()
            .map(|cfd| {
                let pending_proposal = self.pending_proposals.get(&cfd.order.id);
                to_dto_cfd(cfd, pending_proposal, current_price, network)
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
            price: order.price.into(),
            min_quantity: order.min_quantity.into(),
            max_quantity: order.max_quantity.into(),
            leverage: order.leverage,
            liquidation_price: order.liquidation_price.into(),
            creation_timestamp: order.creation_timestamp,
            settlement_time_interval_in_secs: order
                .settlement_time_interval_hours
                .whole_seconds()
                .try_into()
                .expect("settlement_time_interval_hours is always positive number"),
        });

        Event::json(&order).event("order")
    }
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

impl ToSseEvent for bitmex_price_feed::Quote {
    fn to_sse_event(&self) -> Event {
        let quote = Quote {
            bid: self.bid.into(),
            ask: self.ask.into(),
            last_updated_at: self.timestamp,
        };
        Event::json(&quote).event("quote")
    }
}
