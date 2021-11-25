use std::collections::HashMap;

use crate::model::{TakerId, Timestamp};
use crate::{bitmex_price_feed, model};
use crate::{Cfd, Order, UpdateCfdProposals};
use rust_decimal::Decimal;
use serde::Serialize;
use tokio::sync::watch;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    tx: Tx,
}

pub struct Feeds {
    pub order: watch::Receiver<Option<Order>>,
    pub cfds: watch::Receiver<Vec<Cfd>>,
    pub quote: watch::Receiver<Quote>,
    pub settlements: watch::Receiver<UpdateCfdProposals>,
    pub connected_takers: watch::Receiver<Vec<TakerId>>,
}

impl Actor {
    pub fn new(init_cfds: Vec<Cfd>, init_quote: bitmex_price_feed::Quote) -> (Self, Feeds) {
        let (tx_cfds, rx_cfds) = watch::channel(init_cfds);
        let (tx_order, rx_order) = watch::channel(None);
        let (tx_update_cfd_feed, rx_update_cfd_feed) = watch::channel(HashMap::new());
        let (tx_quote, rx_quote) = watch::channel(init_quote.into());
        let (tx_connected_takers, rx_connected_takers) = watch::channel(Vec::new());

        (
            Self {
                tx: Tx {
                    cfds: tx_cfds,
                    order: tx_order,
                    quote: tx_quote,
                    settlements: tx_update_cfd_feed,
                    connected_takers: tx_connected_takers,
                },
            },
            Feeds {
                cfds: rx_cfds,
                order: rx_order,
                quote: rx_quote,
                settlements: rx_update_cfd_feed,
                connected_takers: rx_connected_takers,
            },
        )
    }
}

/// Internal struct to keep all the senders around in one place
struct Tx {
    pub cfds: watch::Sender<Vec<Cfd>>,
    pub order: watch::Sender<Option<Order>>,
    pub quote: watch::Sender<Quote>,
    pub settlements: watch::Sender<UpdateCfdProposals>,
    // TODO: Use this channel to communicate maker status as well with generic
    // ID of connected counterparties
    pub connected_takers: watch::Sender<Vec<TakerId>>,
}

pub struct Update<T>(pub T);

#[xtra_productivity]
impl Actor {
    fn handle(&mut self, msg: Update<Vec<Cfd>>) {
        let _ = self.tx.cfds.send(msg.0);
    }
    fn handle(&mut self, msg: Update<Option<Order>>) {
        let _ = self.tx.order.send(msg.0);
    }
    fn handle(&mut self, msg: Update<bitmex_price_feed::Quote>) {
        let _ = self.tx.quote.send(msg.0.into());
    }
    fn handle(&mut self, msg: Update<UpdateCfdProposals>) {
        let _ = self.tx.settlements.send(msg.0);
    }
    fn handle(&mut self, msg: Update<Vec<TakerId>>) {
        let _ = self.tx.connected_takers.send(msg.0);
    }
}

impl xtra::Actor for Actor {}

/// Types

#[derive(Debug, Clone)]
pub struct Usd {
    inner: model::Usd,
}

impl Usd {
    fn new(usd: model::Usd) -> Self {
        Self {
            inner: model::Usd::new(usd.into_decimal().round_dp(2)),
        }
    }
}

impl From<model::Usd> for Usd {
    fn from(usd: model::Usd) -> Self {
        Self::new(usd)
    }
}

impl Serialize for Usd {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        <Decimal as Serialize>::serialize(&self.inner.into_decimal(), serializer)
    }
}

impl Serialize for Price {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        <Decimal as Serialize>::serialize(&self.inner.into_decimal(), serializer)
    }
}

#[derive(Debug, Clone)]
pub struct Price {
    inner: model::Price,
}

impl Price {
    fn new(price: model::Price) -> Self {
        Self {
            inner: model::Price::new(price.into_decimal().round_dp(2)).expect(
                "rounding a valid price to 2 decimal places should still result in a valid price",
            ),
        }
    }
}

impl From<model::Price> for Price {
    fn from(price: model::Price) -> Self {
        Self::new(price)
    }
}

// TODO: Remove this after CfdsWithAuxData is removed
impl From<Price> for model::Price {
    fn from(price: Price) -> Self {
        price.inner
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Quote {
    bid: Price,
    ask: Price,
    last_updated_at: Timestamp,
}

impl From<bitmex_price_feed::Quote> for Quote {
    fn from(quote: bitmex_price_feed::Quote) -> Self {
        Quote {
            bid: quote.bid.into(),
            ask: quote.ask.into(),
            last_updated_at: quote.timestamp,
        }
    }
}

// TODO: Remove this after CfdsWithAuxData is removed
impl From<Quote> for bitmex_price_feed::Quote {
    fn from(quote: Quote) -> Self {
        Self {
            timestamp: quote.last_updated_at,
            bid: quote.bid.into(),
            ask: quote.ask.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rust_decimal_macros::dec;
    use serde_test::{assert_ser_tokens, Token};

    #[test]
    fn usd_serializes_with_only_cents() {
        let usd = Usd::new(model::Usd::new(dec!(1000.12345)));

        assert_ser_tokens(&usd, &[Token::Str("1000.12")]);
    }

    #[test]
    fn price_serializes_with_only_cents() {
        let price = Price::new(model::Price::new(dec!(1000.12345)).unwrap());

        assert_ser_tokens(&price, &[Token::Str("1000.12")]);
    }
}
