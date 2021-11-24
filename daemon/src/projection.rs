use crate::bitmex_price_feed::Quote;
use crate::model::TakerId;
use crate::{Cfd, Order, UpdateCfdProposals};
use tokio::sync::watch;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    tx_cfds: watch::Sender<Vec<Cfd>>,
    tx_order: watch::Sender<Option<Order>>,
    tx_quote: watch::Sender<Quote>,
    tx_settlements: watch::Sender<UpdateCfdProposals>,
    // TODO: Use this channel to communicate maker status as well with generic
    // ID of connected counterparties
    tx_connected_takers: watch::Sender<Vec<TakerId>>,
}

impl Actor {
    pub fn new(
        tx_cfds: watch::Sender<Vec<Cfd>>,
        tx_order: watch::Sender<Option<Order>>,
        tx_quote: watch::Sender<Quote>,
        tx_settlements: watch::Sender<UpdateCfdProposals>,
        tx_connected_takers: watch::Sender<Vec<TakerId>>,
    ) -> Self {
        Self {
            tx_cfds,
            tx_order,
            tx_quote,
            tx_settlements,
            tx_connected_takers,
        }
    }
}

pub struct Update<T>(pub T);

#[xtra_productivity]
impl Actor {
    fn handle(&mut self, msg: Update<Vec<Cfd>>) {
        let _ = self.tx_cfds.send(msg.0);
    }
    fn handle(&mut self, msg: Update<Option<Order>>) {
        let _ = self.tx_order.send(msg.0);
    }
    fn handle(&mut self, msg: Update<Quote>) {
        let _ = self.tx_quote.send(msg.0);
    }
    fn handle(&mut self, msg: Update<UpdateCfdProposals>) {
        let _ = self.tx_settlements.send(msg.0);
    }
    fn handle(&mut self, msg: Update<Vec<TakerId>>) {
        let _ = self.tx_connected_takers.send(msg.0);
    }
}

impl xtra::Actor for Actor {}
