use crate::connection::ConnectionStatus;
use crate::model::cfd::{OrderId, Role, UpdateCfdProposals};
use crate::model::{Leverage, Position, Timestamp, TradingPair};
use crate::projection::{self, CfdAction, CfdOrder, CfdState, Identity, Price, Quote, Usd};
use crate::{bitmex_price_feed, model};
use bdk::bitcoin::{Amount, Network, SignedAmount};
use rocket::request::FromParam;
use rocket::response::stream::Event;
use rust_decimal::Decimal;
use serde::Serialize;
use time::OffsetDateTime;
use tokio::sync::watch;

#[derive(Debug, Clone, Serialize)]
pub struct Cfd {
    pub order_id: OrderId,
    pub initial_price: Price,

    pub leverage: Leverage,
    pub trading_pair: TradingPair,
    pub position: Position,
    pub liquidation_price: Price,

    pub quantity_usd: Usd,

    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub margin: Amount,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub margin_counterparty: Amount,

    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub profit_btc: SignedAmount,
    pub profit_in_percent: String,

    pub state: CfdState,
    pub actions: Vec<CfdAction>,
    pub state_transition_timestamp: i64,

    pub details: projection::CfdDetails,

    #[serde(with = "::time::serde::timestamp")]
    pub expiry_timestamp: OffsetDateTime,
}

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

/// Intermediate struct to able to piggy-back additional information along with
/// cfds, so we can avoid a 1:1 mapping between the states in the model and seen
/// by UI
pub struct CfdsWithAuxData {
    pub cfds: Vec<model::cfd::Cfd>,
    pub current_price: model::Price,
    pub pending_proposals: UpdateCfdProposals,
    pub network: Network,
}

impl CfdsWithAuxData {
    pub fn new(
        rx_cfds: &watch::Receiver<Vec<model::cfd::Cfd>>,
        rx_quote: &watch::Receiver<Quote>,
        rx_updates: &watch::Receiver<UpdateCfdProposals>,
        role: Role,
        network: Network,
    ) -> Self {
        let quote: bitmex_price_feed::Quote = rx_quote.borrow().clone().into();
        let current_price = match role {
            Role::Maker => quote.for_maker(),
            Role::Taker => quote.for_taker(),
        };

        let pending_proposals = rx_updates.borrow().clone();

        CfdsWithAuxData {
            cfds: rx_cfds.borrow().clone(),
            current_price,
            pending_proposals,
            network,
        }
    }
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
                let (profit_btc, profit_in_percent) =
                    cfd.profit(current_price).unwrap_or_else(|error| {
                        tracing::warn!(
                            "Calculating profit/loss failed. Falling back to 0. {:#}",
                            error
                        );
                        (SignedAmount::ZERO, Decimal::ZERO.into())
                    });

                let pending_proposal = self.pending_proposals.get(&cfd.order.id);
                let state = projection::to_cfd_state(&cfd.state, pending_proposal);

                Cfd {
                    order_id: cfd.order.id,
                    initial_price: cfd.order.price.into(),
                    leverage: cfd.order.leverage,
                    trading_pair: cfd.order.trading_pair.clone(),
                    position: cfd.position(),
                    liquidation_price: cfd.order.liquidation_price.into(),
                    quantity_usd: cfd.quantity_usd.into(),
                    profit_btc,
                    profit_in_percent: profit_in_percent.round_dp(1).to_string(),
                    state: state.clone(),
                    actions: projection::available_actions(state, cfd.role()),
                    state_transition_timestamp: cfd.state.get_transition_timestamp().seconds(),

                    // TODO: Depending on the state the margin might be set (i.e. in Open we save it
                    // in the DB internally) and does not have to be calculated
                    margin: cfd.margin().expect("margin to be available"),
                    margin_counterparty: cfd.counterparty_margin().expect("margin to be available"),
                    details: projection::to_cfd_details(cfd, network),
                    expiry_timestamp: match cfd.expiry_timestamp() {
                        None => cfd.order.oracle_event_id.timestamp(),
                        Some(timestamp) => timestamp,
                    },
                }
            })
            .collect::<Vec<Cfd>>();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_snapshot_test() {
        // Make sure to update the UI after changing this test!

        let json = serde_json::to_string(&CfdState::OutgoingOrderRequest).unwrap();
        assert_eq!(json, "\"OutgoingOrderRequest\"");
        let json = serde_json::to_string(&CfdState::IncomingOrderRequest).unwrap();
        assert_eq!(json, "\"IncomingOrderRequest\"");
        let json = serde_json::to_string(&CfdState::Accepted).unwrap();
        assert_eq!(json, "\"Accepted\"");
        let json = serde_json::to_string(&CfdState::Rejected).unwrap();
        assert_eq!(json, "\"Rejected\"");
        let json = serde_json::to_string(&CfdState::ContractSetup).unwrap();
        assert_eq!(json, "\"ContractSetup\"");
        let json = serde_json::to_string(&CfdState::PendingOpen).unwrap();
        assert_eq!(json, "\"PendingOpen\"");
        let json = serde_json::to_string(&CfdState::Open).unwrap();
        assert_eq!(json, "\"Open\"");
        let json = serde_json::to_string(&CfdState::OpenCommitted).unwrap();
        assert_eq!(json, "\"OpenCommitted\"");
        let json = serde_json::to_string(&CfdState::PendingRefund).unwrap();
        assert_eq!(json, "\"PendingRefund\"");
        let json = serde_json::to_string(&CfdState::Refunded).unwrap();
        assert_eq!(json, "\"Refunded\"");
        let json = serde_json::to_string(&CfdState::SetupFailed).unwrap();
        assert_eq!(json, "\"SetupFailed\"");
    }
}
