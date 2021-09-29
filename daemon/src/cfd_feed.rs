use crate::db::load_all_cfds;
use crate::model::cfd::OrderId;
use crate::model::{Leverage, Position, TradingPair, Usd};
use crate::{bitmex_price_feed, model};
use anyhow::{Context as _, Result};
use bdk::bitcoin::Amount;
use serde::Serialize;
use sqlx::pool::PoolConnection;
use sqlx::Sqlite;
use std::time::UNIX_EPOCH;
use tokio::sync::watch;

#[allow(dead_code)]
/// Role of the actor's owner in the upcoming contract
pub enum Role {
    Maker,
    Taker,
}

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

pub struct CfdFeed {
    role: Role,
    cfd_feed_sender: watch::Sender<Vec<Cfd>>,
    current_price: Option<bitmex_price_feed::Quote>,
}

impl CfdFeed {
    pub fn new(role: Role) -> (Self, watch::Receiver<Vec<Cfd>>) {
        let (cfd_feed_sender, cfd_feed_receiver) = watch::channel::<Vec<Cfd>>(vec![]);
        (
            Self {
                role,
                cfd_feed_sender,
                current_price: None,
            },
            cfd_feed_receiver,
        )
    }

    /// Update price from BitMex. It is recommended to call `update()` afterwards.
    pub fn set_current_price(&mut self, quote: bitmex_price_feed::Quote) {
        self.current_price = Some(quote);
    }

    /// Updates the CFD feed in the frontend
    pub async fn update(&self, conn: &mut PoolConnection<Sqlite>) -> Result<()> {
        let cfds = load_all_cfds(conn).await?;

        let cfds = cfds
            .iter()
            .map(|cfd| {
                let (profit_btc, profit_usd) = self.calculate_profit(cfd);

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

        self.cfd_feed_sender
            .send(cfds)
            .context("Could not update CFD feed")
    }

    fn calculate_profit(&self, cfd: &model::cfd::Cfd) -> (Amount, Usd) {
        if let Some(quote) = &self.current_price {
            cfd.profit(match self.role {
                Role::Taker => quote.for_taker(),
                Role::Maker => quote.for_maker(),
            })
            .unwrap()
        } else {
            // No current price set yet, returning no profits
            (Amount::ZERO, Usd::ZERO)
        }
    }
}
