use std::collections::HashMap;

use crate::bitmex_price_feed::Quote;
use crate::model::TakerId;
use crate::{Cfd, Order, UpdateCfdProposals};
use tokio::sync::watch;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    tx: Tx,
}

pub struct Feeds {
    pub cfds: watch::Receiver<Vec<Cfd>>,
    pub order: watch::Receiver<Option<Order>>,
    pub quote: watch::Receiver<Quote>,
    pub settlements: watch::Receiver<UpdateCfdProposals>,
    pub connected_takers: watch::Receiver<Vec<TakerId>>,
}

impl Actor {
    pub fn new(init_cfds: Vec<Cfd>, init_quote: Quote) -> (Self, Feeds) {
        let (tx_cfds, rx_cfds) = watch::channel(init_cfds);
        let (tx_order, rx_order) = watch::channel(None);
        let (tx_update_cfd_feed, rx_update_cfd_feed) = watch::channel(HashMap::new());
        let (tx_quote, rx_quote) = watch::channel(init_quote);
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
    fn handle(&mut self, msg: Update<Quote>) {
        let _ = self.tx.quote.send(msg.0);
    }
    fn handle(&mut self, msg: Update<UpdateCfdProposals>) {
        let _ = self.tx.settlements.send(msg.0);
    }
    fn handle(&mut self, msg: Update<Vec<TakerId>>) {
        let _ = self.tx.connected_takers.send(msg.0);
    }
}

impl xtra::Actor for Actor {}
