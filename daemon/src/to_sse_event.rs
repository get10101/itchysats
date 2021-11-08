use crate::model::cfd::{
    Dlc, OrderId, Payout, Role, SettlementKind, UpdateCfdProposal, UpdateCfdProposals,
};
use crate::model::{Leverage, Position, Timestamp, TradingPair};
use crate::{bitmex_price_feed, model};
use bdk::bitcoin::{Amount, Network, SignedAmount, Txid};
use rocket::request::FromParam;
use rocket::response::stream::Event;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::convert::TryInto;
use time::OffsetDateTime;
use tokio::sync::watch;

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

impl Serialize for Price {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        <Decimal as Serialize>::serialize(&self.inner.into_decimal(), serializer)
    }
}

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

    pub details: CfdDetails,

    #[serde(with = "::time::serde::timestamp")]
    pub expiry_timestamp: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize)]
pub struct CfdDetails {
    tx_url_list: Vec<TxUrl>,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc::opt")]
    payout: Option<Amount>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TxUrl {
    pub label: TxLabel,
    pub url: String,
}

impl TxUrl {
    pub fn new(txid: Txid, network: Network, label: TxLabel) -> Self {
        Self {
            label,
            url: match network {
                Network::Bitcoin => format!("https://mempool.space/tx/{}", txid),
                Network::Testnet => format!("https://mempool.space/testnet/tx/{}", txid),
                Network::Signet => format!("https://mempool.space/signet/tx/{}", txid),
                Network::Regtest => txid.to_string(),
            },
        }
    }
}

struct TxUrlBuilder {
    network: Network,
}

impl TxUrlBuilder {
    pub fn new(network: Network) -> Self {
        Self { network }
    }

    pub fn lock(&self, dlc: &Dlc) -> TxUrl {
        TxUrl::new(dlc.lock.0.txid(), self.network, TxLabel::Lock)
    }

    pub fn commit(&self, dlc: &Dlc) -> TxUrl {
        TxUrl::new(dlc.commit.0.txid(), self.network, TxLabel::Commit)
    }

    pub fn cet(&self, txid: Txid) -> TxUrl {
        TxUrl::new(txid, self.network, TxLabel::Cet)
    }

    pub fn collaborative_close(&self, txid: Txid) -> TxUrl {
        TxUrl::new(txid, self.network, TxLabel::Collaborative)
    }

    pub fn refund(&self, dlc: &Dlc) -> TxUrl {
        TxUrl::new(dlc.refund.0.txid(), self.network, TxLabel::Refund)
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum TxLabel {
    Lock,
    Commit,
    Cet,
    Refund,
    Collaborative,
}

#[derive(Debug, derive_more::Display, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum CfdAction {
    AcceptOrder,
    RejectOrder,
    Commit,
    Settle,
    AcceptSettlement,
    RejectSettlement,
    AcceptRollOver,
    RejectRollOver,
}

impl<'v> FromParam<'v> for CfdAction {
    type Error = serde_plain::Error;

    fn from_param(param: &'v str) -> Result<Self, Self::Error> {
        let action = serde_plain::from_str(param)?;
        Ok(action)
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum CfdState {
    OutgoingOrderRequest,
    IncomingOrderRequest,
    Accepted,
    Rejected,
    ContractSetup,
    PendingOpen,
    Open,
    PendingCommit,
    PendingCet,
    PendingClose,
    OpenCommitted,
    IncomingSettlementProposal,
    OutgoingSettlementProposal,
    IncomingRollOverProposal,
    OutgoingRollOverProposal,
    Closed,
    PendingRefund,
    Refunded,
    SetupFailed,
}

#[derive(Debug, Clone, Serialize)]
pub struct CfdOrder {
    pub id: OrderId,

    pub trading_pair: TradingPair,
    pub position: Position,

    pub price: Price,

    pub min_quantity: Usd,
    pub max_quantity: Usd,

    pub leverage: Leverage,
    pub liquidation_price: Price,

    pub creation_timestamp: Timestamp,
    pub settlement_time_interval_in_secs: u64,
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
        rx_quote: &watch::Receiver<bitmex_price_feed::Quote>,
        rx_updates: &watch::Receiver<UpdateCfdProposals>,
        role: Role,
        network: Network,
    ) -> Self {
        let quote = rx_quote.borrow().clone();
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
                let state = to_cfd_state(&cfd.state, pending_proposal);

                let details = CfdDetails {
                    tx_url_list: to_tx_url_list(cfd.state.clone(), network),
                    payout: cfd.payout(),
                };

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
                    actions: available_actions(state, cfd.role()),
                    state_transition_timestamp: cfd.state.get_transition_timestamp().seconds(),

                    // TODO: Depending on the state the margin might be set (i.e. in Open we save it
                    // in the DB internally) and does not have to be calculated
                    margin: cfd.margin().expect("margin to be available"),
                    margin_counterparty: cfd.counterparty_margin().expect("margin to be available"),
                    details,
                    expiry_timestamp: cfd.expiry_timestamp(),
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
            price: order.price.into(),
            min_quantity: order.min_quantity.into(),
            max_quantity: order.max_quantity.into(),
            leverage: order.leverage,
            liquidation_price: order.liquidation_price.into(),
            creation_timestamp: order.creation_timestamp,
            settlement_time_interval_in_secs: order
                .settlement_interval
                .whole_seconds()
                .try_into()
                .expect("settlement_time_interval_hours is always positive number"),
        });

        Event::json(&order).event("order")
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

fn to_cfd_state(
    cfd_state: &model::cfd::CfdState,
    proposal_status: Option<&UpdateCfdProposal>,
) -> CfdState {
    match proposal_status {
        Some(UpdateCfdProposal::Settlement {
            direction: SettlementKind::Outgoing,
            ..
        }) => CfdState::OutgoingSettlementProposal,
        Some(UpdateCfdProposal::Settlement {
            direction: SettlementKind::Incoming,
            ..
        }) => CfdState::IncomingSettlementProposal,
        Some(UpdateCfdProposal::RollOverProposal {
            direction: SettlementKind::Outgoing,
            ..
        }) => CfdState::OutgoingRollOverProposal,
        Some(UpdateCfdProposal::RollOverProposal {
            direction: SettlementKind::Incoming,
            ..
        }) => CfdState::IncomingRollOverProposal,
        None => match cfd_state {
            // Filled in collaborative close in Open means that we're awaiting
            // a collaborative closure
            model::cfd::CfdState::Open {
                collaborative_close: Some(_),
                ..
            } => CfdState::PendingClose,
            model::cfd::CfdState::OutgoingOrderRequest { .. } => CfdState::OutgoingOrderRequest,
            model::cfd::CfdState::IncomingOrderRequest { .. } => CfdState::IncomingOrderRequest,
            model::cfd::CfdState::Accepted { .. } => CfdState::Accepted,
            model::cfd::CfdState::Rejected { .. } => CfdState::Rejected,
            model::cfd::CfdState::ContractSetup { .. } => CfdState::ContractSetup,
            model::cfd::CfdState::PendingOpen { .. } => CfdState::PendingOpen,
            model::cfd::CfdState::Open { .. } => CfdState::Open,
            model::cfd::CfdState::OpenCommitted { .. } => CfdState::OpenCommitted,
            model::cfd::CfdState::PendingRefund { .. } => CfdState::PendingRefund,
            model::cfd::CfdState::Refunded { .. } => CfdState::Refunded,
            model::cfd::CfdState::SetupFailed { .. } => CfdState::SetupFailed,
            model::cfd::CfdState::PendingCommit { .. } => CfdState::PendingCommit,
            model::cfd::CfdState::PendingCet { .. } => CfdState::PendingCet,
            model::cfd::CfdState::Closed { .. } => CfdState::Closed,
        },
    }
}

fn to_tx_url_list(state: model::cfd::CfdState, network: Network) -> Vec<TxUrl> {
    use model::cfd::CfdState::*;

    let tx_ub = TxUrlBuilder::new(network);

    match state {
        PendingOpen { dlc, .. } => {
            vec![tx_ub.lock(&dlc)]
        }
        PendingCommit { dlc, .. } => vec![tx_ub.lock(&dlc), tx_ub.commit(&dlc)],
        OpenCommitted { dlc, .. } => vec![tx_ub.lock(&dlc), tx_ub.commit(&dlc)],
        Open {
            dlc,
            collaborative_close,
            ..
        } => {
            let mut tx_urls = vec![tx_ub.lock(&dlc)];
            if let Some(collaborative_close) = collaborative_close {
                tx_urls.push(tx_ub.collaborative_close(collaborative_close.tx.txid()));
            }
            tx_urls
        }
        PendingCet {
            dlc, attestation, ..
        } => vec![
            tx_ub.lock(&dlc),
            tx_ub.commit(&dlc),
            tx_ub.cet(attestation.txid()),
        ],
        Closed {
            payout: Payout::Cet(attestation),
            ..
        } => vec![tx_ub.cet(attestation.txid())],
        Closed {
            payout: Payout::CollaborativeClose(collaborative_close),
            ..
        } => {
            vec![tx_ub.collaborative_close(collaborative_close.tx.txid())]
        }
        PendingRefund { dlc, .. } => vec![tx_ub.lock(&dlc), tx_ub.commit(&dlc), tx_ub.refund(&dlc)],
        Refunded { dlc, .. } => vec![tx_ub.refund(&dlc)],
        OutgoingOrderRequest { .. }
        | IncomingOrderRequest { .. }
        | Accepted { .. }
        | Rejected { .. }
        | ContractSetup { .. }
        | SetupFailed { .. } => vec![],
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Quote {
    bid: Price,
    ask: Price,
    last_updated_at: Timestamp,
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

fn available_actions(state: CfdState, role: Role) -> Vec<CfdAction> {
    match (state, role) {
        (CfdState::IncomingOrderRequest { .. }, Role::Maker) => {
            vec![CfdAction::AcceptOrder, CfdAction::RejectOrder]
        }
        (CfdState::IncomingSettlementProposal { .. }, Role::Maker) => {
            vec![CfdAction::AcceptSettlement, CfdAction::RejectSettlement]
        }
        (CfdState::IncomingRollOverProposal { .. }, Role::Maker) => {
            vec![CfdAction::AcceptRollOver, CfdAction::RejectRollOver]
        }
        // If there is an outgoing settlement proposal already, user can't
        // initiate new one
        (CfdState::OutgoingSettlementProposal { .. }, Role::Maker) => {
            vec![CfdAction::Commit]
        }
        // User is awaiting collaborative close, commit is left as a safeguard
        (CfdState::PendingClose { .. }, _) => {
            vec![CfdAction::Commit]
        }
        (CfdState::Open { .. }, Role::Taker) => {
            vec![CfdAction::Commit, CfdAction::Settle]
        }
        (CfdState::Open { .. }, Role::Maker) => vec![CfdAction::Commit],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rust_decimal_macros::dec;
    use serde_test::{assert_ser_tokens, Token};

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
