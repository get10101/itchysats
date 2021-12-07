use std::collections::HashMap;

use crate::model::cfd::{
    Cfd as ModelCfd, OrderId, Role, RollOverProposal, SettlementKind, SettlementProposal,
    UpdateCfdProposal,
};
use crate::model::{Leverage, Position, Timestamp, TradingPair};
use crate::{bitmex_price_feed, db, model, tx, Order, UpdateCfdProposals};
use anyhow::Result;
use bdk::bitcoin::{Amount, Network, SignedAmount};
use itertools::Itertools;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::watch;
use xtra_productivity::xtra_productivity;

/// Amend a given settlement proposal (if `proposal.is_none()`, it should be removed)
pub struct UpdateSettlementProposal {
    pub order: OrderId,
    pub proposal: Option<(SettlementProposal, SettlementKind)>,
}

/// Amend a given rollover proposal (if `proposal.is_none()`, it should be removed)
pub struct UpdateRollOverProposal {
    pub order: OrderId,
    pub proposal: Option<(RollOverProposal, SettlementKind)>,
}

/// Store the latest state of `T` for display purposes
/// (replaces previously stored values)
pub struct Update<T>(pub T);

/// Message indicating that the Cfds in the projection need to be reloaded, as at
/// least one of the Cfds has changed.
pub struct CfdsChanged;

pub struct Actor {
    db: sqlx::SqlitePool,
    tx: Tx,
    state: State,
}

pub struct Feeds {
    pub quote: watch::Receiver<Quote>,
    pub order: watch::Receiver<Option<CfdOrder>>,
    pub connected_takers: watch::Receiver<Vec<Identity>>,
    pub cfds: watch::Receiver<Vec<Cfd>>,
}

impl Actor {
    pub async fn new(
        db: sqlx::SqlitePool,
        role: Role,
        network: Network,
        init_quote: bitmex_price_feed::Quote,
    ) -> Result<(Self, Feeds)> {
        let mut conn = db.acquire().await?;
        let init_cfds = db::load_all_cfds(&mut conn).await?;

        let state = State {
            role,
            network,
            cfds: init_cfds,
            proposals: HashMap::new(),
            quote: init_quote.clone(),
        };

        let (tx_cfds, rx_cfds) = watch::channel(state.to_cfds());
        let (tx_order, rx_order) = watch::channel(None);
        let (tx_quote, rx_quote) = watch::channel(init_quote.into());
        let (tx_connected_takers, rx_connected_takers) = watch::channel(Vec::new());

        Ok((
            Self {
                db,
                tx: Tx {
                    cfds: tx_cfds,
                    order: tx_order,
                    quote: tx_quote,
                    connected_takers: tx_connected_takers,
                },
                state,
            },
            Feeds {
                cfds: rx_cfds,
                order: rx_order,
                quote: rx_quote,
                connected_takers: rx_connected_takers,
            },
        ))
    }
}

/// Internal struct to keep all the senders around in one place
struct Tx {
    pub cfds: watch::Sender<Vec<Cfd>>,
    pub order: watch::Sender<Option<CfdOrder>>,
    pub quote: watch::Sender<Quote>,
    // TODO: Use this channel to communicate maker status as well with generic
    // ID of connected counterparties
    pub connected_takers: watch::Sender<Vec<Identity>>,
}

/// Internal struct to keep state in one place
struct State {
    role: Role,
    network: Network,
    quote: bitmex_price_feed::Quote,
    proposals: UpdateCfdProposals,
    cfds: Vec<ModelCfd>,
}

impl State {
    pub fn to_cfds(&self) -> Vec<Cfd> {
        // FIXME: starting with the intermediate struct, only temporarily
        let temp = CfdsWithAuxData::new(
            self.cfds.clone(),
            self.quote.clone(),
            self.proposals.clone(),
            self.role,
            self.network,
        );
        temp.into()
    }

    pub fn amend_settlement_proposal(&mut self, proposal: UpdateSettlementProposal) {
        let order = proposal.order;
        self.amend_cfd_proposal(order, proposal.into())
    }

    pub fn amend_rollover_proposal(&mut self, proposal: UpdateRollOverProposal) {
        let order = proposal.order;
        self.amend_cfd_proposal(order, proposal.into())
    }

    pub fn update_quote(&mut self, quote: bitmex_price_feed::Quote) {
        let _ = std::mem::replace(&mut self.quote, quote);
    }

    pub fn update_cfds(&mut self, cfds: Vec<ModelCfd>) {
        let _ = std::mem::replace(&mut self.cfds, cfds);
    }

    fn amend_cfd_proposal(&mut self, order: OrderId, proposal: Option<UpdateCfdProposal>) {
        if let Some(proposal) = proposal {
            self.proposals.insert(order, proposal);
            tracing::trace!(%order, "Cfd proposal got updated");

            return;
        }

        if self.proposals.remove(&order).is_none() {
            tracing::trace!(%order, "Cannot remove cfd proposal: unknown");

            return;
        }

        tracing::trace!(%order, "Removed cfd proposal");
    }
}

#[xtra_productivity]
impl Actor {
    fn handle(&mut self, _: CfdsChanged) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        let cfds = db::load_all_cfds(&mut conn).await?;
        self.state.update_cfds(cfds);
        let _ = self.tx.cfds.send(self.state.to_cfds());
        Ok(())
    }
    fn handle(&mut self, msg: Update<Option<Order>>) {
        let _ = self.tx.order.send(msg.0.map(|x| x.into()));
    }
    fn handle(&mut self, msg: Update<bitmex_price_feed::Quote>) {
        let quote = msg.0;
        self.state.update_quote(quote.clone());
        let _ = self.tx.quote.send(quote.into());
        let _ = self.tx.cfds.send(self.state.to_cfds());
    }
    fn handle(&mut self, msg: Update<Vec<model::Identity>>) {
        let _ = self
            .tx
            .connected_takers
            .send(msg.0.iter().map(|x| x.into()).collect_vec());
    }
    fn handle(&mut self, msg: UpdateSettlementProposal) {
        self.state.amend_settlement_proposal(msg);
        let _ = self.tx.cfds.send(self.state.to_cfds());
    }
    fn handle(&mut self, msg: UpdateRollOverProposal) {
        self.state.amend_rollover_proposal(msg);
        let _ = self.tx.cfds.send(self.state.to_cfds());
    }
}

impl xtra::Actor for Actor {}

/// Types

#[derive(Debug, Clone, PartialEq)]
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

#[derive(Debug, Clone, PartialEq)]
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

#[derive(Debug, Clone, PartialEq, Serialize)]
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

impl From<Order> for CfdOrder {
    fn from(order: Order) -> Self {
        Self {
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
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, derive_more::Display)]
pub struct Identity(String);

impl From<&model::Identity> for Identity {
    fn from(id: &model::Identity) -> Self {
        Self(id.to_string())
    }
}

impl From<model::Identity> for Identity {
    fn from(id: model::Identity) -> Self {
        Self(id.to_string())
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

pub fn to_cfd_state(
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

#[derive(Debug, Clone, Serialize)]
pub struct CfdDetails {
    tx_url_list: Vec<tx::TxUrl>,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc::opt")]
    payout: Option<Amount>,
}

fn to_cfd_details(cfd: &model::cfd::Cfd, network: Network) -> CfdDetails {
    CfdDetails {
        tx_url_list: tx::to_tx_url_list(cfd.state.clone(), network),
        payout: cfd.payout(),
    }
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
    RollOver,
    AcceptRollOver,
    RejectRollOver,
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
            vec![CfdAction::RollOver, CfdAction::Commit, CfdAction::Settle]
        }
        (CfdState::Open { .. }, Role::Maker) => vec![CfdAction::Commit],
        _ => vec![],
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
    pub profit_percent: String,

    pub state: CfdState,
    pub actions: Vec<CfdAction>,
    pub state_transition_timestamp: i64,

    pub details: CfdDetails,

    #[serde(with = "::time::serde::timestamp")]
    pub expiry_timestamp: OffsetDateTime,
}

impl From<CfdsWithAuxData> for Vec<Cfd> {
    fn from(input: CfdsWithAuxData) -> Self {
        let current_price = input.current_price;
        let network = input.network;

        let cfds = input
            .cfds
            .iter()
            .map(|cfd| {
                let (profit_btc, profit_percent) =
                    cfd.profit(current_price).unwrap_or_else(|error| {
                        tracing::warn!(
                            "Calculating profit/loss failed. Falling back to 0. {:#}",
                            error
                        );
                        (SignedAmount::ZERO, Decimal::ZERO.into())
                    });

                let pending_proposal = input.pending_proposals.get(&cfd.order.id);
                let state = to_cfd_state(&cfd.state, pending_proposal);

                Cfd {
                    order_id: cfd.order.id,
                    initial_price: cfd.order.price.into(),
                    leverage: cfd.order.leverage,
                    trading_pair: cfd.order.trading_pair.clone(),
                    position: cfd.position(),
                    liquidation_price: cfd.order.liquidation_price.into(),
                    quantity_usd: cfd.quantity_usd.into(),
                    profit_btc,
                    profit_percent: profit_percent.round_dp(1).to_string(),
                    state: state.clone(),
                    actions: available_actions(state, cfd.role()),
                    state_transition_timestamp: cfd.state.get_transition_timestamp().seconds(),

                    // TODO: Depending on the state the margin might be set (i.e. in Open we save it
                    // in the DB internally) and does not have to be calculated
                    margin: cfd.margin().expect("margin to be available"),
                    margin_counterparty: cfd.counterparty_margin().expect("margin to be available"),
                    details: to_cfd_details(cfd, network),
                    expiry_timestamp: match cfd.expiry_timestamp() {
                        None => cfd.order.oracle_event_id.timestamp(),
                        Some(timestamp) => timestamp,
                    },
                }
            })
            .collect::<Vec<Cfd>>();
        cfds
    }
}

/// Intermediate struct to able to piggy-back additional information along with
/// cfds, so we can avoid a 1:1 mapping between the states in the model and seen
/// by UI
// TODO: Remove this struct out of existence
pub struct CfdsWithAuxData {
    pub cfds: Vec<model::cfd::Cfd>,
    pub current_price: model::Price,
    pub pending_proposals: UpdateCfdProposals,
    pub network: Network,
}

impl CfdsWithAuxData {
    pub fn new(
        cfds: Vec<model::cfd::Cfd>,
        quote: bitmex_price_feed::Quote,
        pending_proposals: UpdateCfdProposals,
        role: Role,
        network: Network,
    ) -> Self {
        let current_price = match role {
            Role::Maker => quote.for_maker(),
            Role::Taker => quote.for_taker(),
        };

        CfdsWithAuxData {
            cfds,
            current_price,
            pending_proposals,
            network,
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

pub fn try_into_update_settlement_proposal(
    cfd_update_proposal: UpdateCfdProposal,
) -> Result<UpdateSettlementProposal> {
    match cfd_update_proposal {
        UpdateCfdProposal::Settlement {
            proposal,
            direction,
        } => Ok(UpdateSettlementProposal {
            order: proposal.order_id,
            proposal: Some((proposal, direction)),
        }),
        UpdateCfdProposal::RollOverProposal { .. } => {
            anyhow::bail!("Can't convert a RollOver proposal")
        }
    }
}

pub fn try_into_update_rollover_proposal(
    cfd_update_proposal: UpdateCfdProposal,
) -> Result<UpdateRollOverProposal> {
    match cfd_update_proposal {
        UpdateCfdProposal::RollOverProposal {
            proposal,
            direction,
        } => Ok(UpdateRollOverProposal {
            order: proposal.order_id,
            proposal: Some((proposal, direction)),
        }),
        UpdateCfdProposal::Settlement { .. } => {
            anyhow::bail!("Can't convert a Settlement proposal")
        }
    }
}

impl From<UpdateSettlementProposal> for Option<UpdateCfdProposal> {
    fn from(proposal: UpdateSettlementProposal) -> Self {
        let UpdateSettlementProposal { order: _, proposal } = proposal;
        if let Some((proposal, kind)) = proposal {
            Some(UpdateCfdProposal::Settlement {
                proposal,
                direction: kind,
            })
        } else {
            None
        }
    }
}

impl From<UpdateRollOverProposal> for Option<UpdateCfdProposal> {
    fn from(proposal: UpdateRollOverProposal) -> Self {
        let UpdateRollOverProposal { order: _, proposal } = proposal;
        if let Some((proposal, kind)) = proposal {
            Some(UpdateCfdProposal::RollOverProposal {
                proposal,
                direction: kind,
            })
        } else {
            None
        }
    }
}
