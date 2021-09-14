use crate::model;
use crate::model::cfd::CfdOfferId;
use crate::model::{Leverage, Position, TradingPair, Usd};
use bdk::bitcoin::Amount;
use rocket::response::stream::Event;
use serde::Serialize;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Serialize)]
pub struct Cfd {
    pub offer_id: CfdOfferId,
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
    pub state_transition_unix_timestamp: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CfdOffer {
    pub id: CfdOfferId,

    pub trading_pair: TradingPair,
    pub position: Position,

    pub price: Usd,

    pub min_quantity: Usd,
    pub max_quantity: Usd,

    pub leverage: Leverage,
    pub liquidation_price: Usd,

    pub creation_unix_timestamp: u64,
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
                let (profit_btc, profit_usd) = cfd.calc_profit(current_price).unwrap();

                Cfd {
                    offer_id: cfd.offer_id,
                    initial_price: cfd.initial_price,
                    leverage: cfd.leverage,
                    trading_pair: cfd.trading_pair.clone(),
                    position: cfd.position.clone(),
                    liquidation_price: cfd.liquidation_price,
                    quantity_usd: cfd.quantity_usd,
                    profit_btc,
                    profit_usd,
                    state: cfd.state.to_string(),
                    state_transition_unix_timestamp: cfd
                        .state
                        .get_transition_timestamp()
                        .duration_since(UNIX_EPOCH)
                        .expect("timestamp to be convertable to duration since epoch")
                        .as_secs(),

                    // TODO: Depending on the state the margin might be set (i.e. in Open we save it
                    // in the DB internally) and does not have to be calculated
                    margin: cfd.calc_margin().unwrap(),
                }
            })
            .collect::<Vec<Cfd>>();

        Event::json(&cfds).event("cfds")
    }
}

impl ToSseEvent for Option<model::cfd::CfdOffer> {
    fn to_sse_event(&self) -> Event {
        let offer = self.clone().map(|offer| CfdOffer {
            id: offer.id,
            trading_pair: offer.trading_pair,
            position: offer.position,
            price: offer.price,
            min_quantity: offer.min_quantity,
            max_quantity: offer.max_quantity,
            leverage: offer.leverage,
            liquidation_price: offer.liquidation_price,
            creation_unix_timestamp: offer
                .creation_timestamp
                .duration_since(UNIX_EPOCH)
                .expect("timestamp to be convertiblae to dureation since epoch")
                .as_secs(),
            term_in_secs: offer.term.as_secs(),
        });

        Event::json(&offer).event("offer")
    }
}

impl ToSseEvent for Amount {
    fn to_sse_event(&self) -> Event {
        Event::json(&self.as_btc()).event("balance")
    }
}
