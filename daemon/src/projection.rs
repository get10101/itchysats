use crate::db;
use crate::db::Settlement;
use crate::Order;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::Amount;
use bdk::bitcoin::Network;
use bdk::bitcoin::Script;
use bdk::bitcoin::SignedAmount;
use bdk::bitcoin::Transaction;
use bdk::bitcoin::Txid;
use bdk::miniscript::DescriptorTrait;
use core::fmt;
use derivative::Derivative;
use futures::StreamExt;
use itertools::Itertools;
use maia::TransactionExt;
use model::calculate_long_liquidation_price;
use model::calculate_margin;
use model::calculate_profit;
use model::calculate_profit_at_price;
use model::calculate_short_liquidation_price;
use model::long_and_short_leverage;
use model::market_closing_price;
use model::CfdEvent;
use model::Dlc;
use model::EventKind;
use model::FeeAccount;
use model::FundingFee;
use model::FundingRate;
use model::Leverage;
use model::OrderId;
use model::Origin;
use model::Position;
use model::Price;
use model::Role;
use model::Timestamp;
use model::TradingPair;
use model::Usd;
use model::SETTLEMENT_INTERVAL;
use parse_display::Display;
use parse_display::FromStr;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::Neg;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::sync::watch;
use tokio_tasks::Tasks;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;
use xtras::SendAsyncSafe;

/// Store the latest state of `T` for display purposes
/// (replaces previously stored values)
pub struct Update<T>(pub T);

/// Indicates that the CFD with the given order ID changed.
#[derive(Clone, Copy)]
pub struct CfdChanged(pub OrderId);

/// Perform the bulk initialisation of the CFD feed
#[derive(Clone, Copy)]
struct Initialize;

pub struct Actor {
    db: db::Connection,
    tx: Tx,
    state: State,
    price_feed: Box<dyn MessageChannel<xtra_bitmex_price_feed::LatestQuote>>,
    tasks: Tasks,
}

pub struct Feeds {
    pub quote: watch::Receiver<Option<Quote>>,
    pub offers: watch::Receiver<MakerOffers>,
    pub connected_takers: watch::Receiver<Vec<model::Identity>>,
    pub cfds: watch::Receiver<Option<Vec<Cfd>>>,
}

impl Actor {
    pub fn new(
        db: db::Connection,
        network: Network,
        price_feed: &(impl MessageChannel<xtra_bitmex_price_feed::LatestQuote> + 'static),
    ) -> (Self, Feeds) {
        let (tx_cfds, rx_cfds) = watch::channel(None);
        let (tx_order, rx_order) = watch::channel(MakerOffers {
            long: None,
            short: None,
        });
        let (tx_quote, rx_quote) = watch::channel(None);
        let (tx_connected_takers, rx_connected_takers) = watch::channel(Vec::new());

        let actor = Self {
            db,
            tx: Tx {
                cfds: tx_cfds,
                order: tx_order,
                quote: tx_quote,
                connected_takers: tx_connected_takers,
            },
            state: State::new(network),
            price_feed: price_feed.clone_channel(),
            tasks: Tasks::default(),
        };
        let feeds = Feeds {
            cfds: rx_cfds,
            offers: rx_order,
            quote: rx_quote,
            connected_takers: rx_connected_takers,
        };

        (actor, feeds)
    }
}

#[derive(Derivative, Clone, Debug, Serialize)]
#[derivative(PartialEq)]
pub struct Cfd {
    pub order_id: OrderId,
    #[serde(with = "round_to_two_dp")]
    pub initial_price: Price,

    /// Sum of all costs
    ///
    /// Includes the opening fee and all fees that were already charged.
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub accumulated_fees: SignedAmount,

    /// The taker leverage
    #[serde(rename = "leverage")]
    pub leverage_taker: Leverage,
    pub trading_pair: TradingPair,
    pub position: Position,
    #[serde(with = "round_to_two_dp")]
    pub liquidation_price: Price,

    #[serde(with = "round_to_two_dp")]
    pub quantity_usd: Usd,

    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub margin: Amount,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub margin_counterparty: Amount,
    pub role: Role,

    /// Projected or final profit amount
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc::opt")]
    pub profit_btc: Option<SignedAmount>,
    /// Projected or final profit percent
    pub profit_percent: Option<String>,

    // TODO: Payout should not be a signed amount but should be converted to a `bitcoin::Amount`
    // when calculating
    /// Projected or final payout
    ///
    /// If we don't know the final payout yet then we calculate this based on the projected profit.
    /// If we don't have a current price in this scenario we don't know the payout, hence it is
    /// represented as option. If we already know the final payout (based on CET or
    /// collborative close) then this is the final payout.
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc::opt")]
    pub payout: Option<SignedAmount>,
    pub closing_price: Option<Price>,

    pub state: CfdState,
    pub actions: HashSet<CfdAction>,

    // TODO: This `CfdDetails` wrapper is useless and could be removed, but that would be a
    // breaking API change
    pub details: CfdDetails,

    #[serde(with = "::time::serde::timestamp::option")]
    pub expiry_timestamp: Option<OffsetDateTime>,

    pub counterparty: model::Identity,

    #[serde(with = "round_to_two_dp::opt")]
    pub pending_settlement_proposal_price: Option<Price>,

    #[serde(skip)]
    #[derivative(PartialEq = "ignore")]
    aggregated: Aggregated,

    #[serde(skip)]
    network: Network,
}

/// Bundle all state extracted from the events in one struct.
///
/// This struct is not serialized but simply carries all state we are interested in from the events.
/// The [`Cfd`] struct above fulfills two roles currently:
/// - It represents the API model that is serialized.
/// - It serves as an aggregate that is hydrated from events.
///
/// This dual-role motivates the existence of this struct.
#[derive(Clone, Debug)]
struct Aggregated {
    fee_account: FeeAccount,

    /// If this is present, we have an active DLC.
    latest_dlc: Option<Dlc>,
    /// If this is present, it should have been published.
    collab_settlement_tx: Option<(Transaction, Script)>,
    /// If this is present, it should have been published.
    cet: Option<Transaction>,
    /// If this is present, it should have been published.
    refund_tx: Option<Transaction>,

    /// If this is present the cet has not been published
    timelocked_cet: Option<Transaction>,

    commit_published: bool,
    refund_published: bool,

    /// Keep track of persistent state in case a protocol fails and we need to
    /// return to previous state
    state: CfdState,

    /// Negotiated states of protocols
    rollover_state: Option<ProtocolNegotiationState>,
    settlement_state: Option<ProtocolNegotiationState>,

    version: u32,
    creation_timestamp: Timestamp,
}

impl Aggregated {
    fn new(fee_account: FeeAccount) -> Self {
        Self {
            fee_account,

            latest_dlc: None,
            collab_settlement_tx: None,
            cet: None,
            refund_tx: None,
            timelocked_cet: None,
            commit_published: false,
            refund_published: false,
            state: CfdState::PendingSetup,
            rollover_state: None,
            settlement_state: None,
            version: 0,
            creation_timestamp: Timestamp::now(),
        }
    }

    fn payout(self, role: Role) -> Option<Amount> {
        if let Some((tx, script)) = self.collab_settlement_tx {
            return Some(extract_payout_amount(tx, script));
        }

        if let Some(tx) = self.refund_tx {
            let script = self.latest_dlc?.script_pubkey_for(role);
            return Some(extract_payout_amount(tx, script));
        }

        let tx = self.cet.or(self.timelocked_cet)?;
        let script = self.latest_dlc?.script_pubkey_for(role);

        Some(extract_payout_amount(tx, script))
    }

    /// Derive Cfd state based on aggregated state from the events and the
    /// protocol state
    fn derive_cfd_state(&self, role: Role) -> CfdState {
        if let Some(settlement_state) = self.settlement_state {
            return match settlement_state {
                ProtocolNegotiationState::Started => match role {
                    Role::Maker => CfdState::IncomingSettlementProposal,
                    Role::Taker => CfdState::OutgoingSettlementProposal,
                },
                ProtocolNegotiationState::Accepted => CfdState::IncomingSettlementProposal,
            };
        };
        if let Some(rollover_state) = self.rollover_state {
            return match rollover_state {
                ProtocolNegotiationState::Started => match role {
                    Role::Maker => CfdState::IncomingRolloverProposal,
                    Role::Taker => CfdState::OutgoingRolloverProposal,
                },
                ProtocolNegotiationState::Accepted => CfdState::RolloverSetup,
            };
        };
        self.state
    }
}

/// Returns output if it can be found or zero amount
///
/// If we cannot find an output for our script we assume that we were liquidated.
fn extract_payout_amount(tx: Transaction, script: Script) -> Amount {
    tx.output
        .into_iter()
        .find(|tx_out| tx_out.script_pubkey == script)
        .map(|tx_out| Amount::from_sat(tx_out.value))
        .unwrap_or(Amount::ZERO)
}

/// Capture state of protocol negotiation for the UI purposes.
#[derive(Clone, Copy, Debug)]
enum ProtocolNegotiationState {
    /// Protocol has been kicked off, likely by user action
    Started,
    /// Other party has agreed to proceed with the protocol
    Accepted,
}

impl Cfd {
    fn new(
        db::Cfd {
            id,
            position,
            initial_price,
            taker_leverage,
            quantity_usd,
            counterparty_network_identity,
            role,
            opening_fee,
            initial_funding_rate,
            ..
        }: db::Cfd,
        network: Network,
    ) -> Self {
        let (our_leverage, counterparty_leverage) = match role {
            Role::Maker => (Leverage::ONE, taker_leverage),
            Role::Taker => (taker_leverage, Leverage::ONE),
        };

        let margin = calculate_margin(initial_price, quantity_usd, our_leverage);
        let margin_counterparty =
            calculate_margin(initial_price, quantity_usd, counterparty_leverage);

        let liquidation_price = match position {
            Position::Long => calculate_long_liquidation_price(our_leverage, initial_price),
            Position::Short => calculate_short_liquidation_price(our_leverage, initial_price),
        };

        let (long_leverage, short_leverage) =
            long_and_short_leverage(taker_leverage, role, position);

        let initial_funding_fee = FundingFee::calculate(
            initial_price,
            quantity_usd,
            long_leverage,
            short_leverage,
            initial_funding_rate,
            SETTLEMENT_INTERVAL.whole_hours(),
        )
        .expect("values from db to be sane");

        let fee_account = FeeAccount::new(position, role)
            .add_opening_fee(opening_fee)
            .add_funding_fee(initial_funding_fee);

        let initial_actions = if role == Role::Maker {
            HashSet::from([CfdAction::AcceptOrder, CfdAction::RejectOrder])
        } else {
            HashSet::new()
        };

        Self {
            order_id: id,
            initial_price,
            accumulated_fees: fee_account.balance(),
            leverage_taker: taker_leverage,
            trading_pair: TradingPair::BtcUsd,
            position,
            liquidation_price,
            quantity_usd,
            margin,
            margin_counterparty,
            role,

            profit_btc: None,
            profit_percent: None,
            payout: None,
            closing_price: None,

            state: CfdState::PendingSetup,
            actions: initial_actions,
            details: CfdDetails {
                tx_url_list: HashSet::new(),
            },
            expiry_timestamp: None,
            counterparty: counterparty_network_identity,
            pending_settlement_proposal_price: None,
            aggregated: Aggregated::new(fee_account),
            network,
        }
    }

    fn apply(mut self, event: CfdEvent) -> Self {
        if self.aggregated.version == 0 {
            self.aggregated.creation_timestamp = event.timestamp;
        }

        // First, try to set state based on event.
        use EventKind::*;
        match event.event {
            ContractSetupStarted => {
                self.aggregated.state = CfdState::ContractSetup;
            }
            ContractSetupCompleted { dlc } => {
                self.expiry_timestamp = Some(dlc.settlement_event_id.timestamp());
                self.aggregated.latest_dlc = Some(dlc);

                self.aggregated.state = CfdState::PendingOpen;
            }
            ContractSetupFailed => {
                self.aggregated.state = CfdState::SetupFailed;
            }
            OfferRejected => {
                self.aggregated.state = CfdState::Rejected;
            }
            RolloverCompleted { dlc, funding_fee } => {
                self.aggregated.rollover_state = None;
                self.expiry_timestamp = Some(dlc.settlement_event_id.timestamp());
                self.aggregated.latest_dlc = Some(dlc);
                self.aggregated.fee_account =
                    self.aggregated.fee_account.add_funding_fee(funding_fee);
                self.accumulated_fees = self.aggregated.fee_account.balance();

                self.aggregated.state = CfdState::Open;
            }
            RolloverRejected => {
                self.aggregated.rollover_state = None;
            }
            RolloverFailed => {
                self.aggregated.rollover_state = None;
            }
            CollaborativeSettlementStarted { proposal } => {
                self.aggregated.settlement_state = Some(ProtocolNegotiationState::Started);
                if let Role::Maker = self.role {
                    self.pending_settlement_proposal_price = Some(proposal.price);
                };
            }
            CollaborativeSettlementProposalAccepted => {
                self.aggregated.settlement_state = Some(ProtocolNegotiationState::Accepted);
                self.pending_settlement_proposal_price = None;
            }
            CollaborativeSettlementCompleted {
                spend_tx,
                script,
                price,
            } => {
                self.aggregated.settlement_state = None;
                self.aggregated.collab_settlement_tx = Some((spend_tx, script));
                self.closing_price = Some(price);

                self.aggregated.state = CfdState::PendingClose;
            }
            CollaborativeSettlementRejected => {
                self.aggregated.settlement_state = None;
                self.pending_settlement_proposal_price = None;
            }
            CollaborativeSettlementFailed => {
                self.aggregated.settlement_state = None;
                self.pending_settlement_proposal_price = None;
            }
            LockConfirmed => {
                self.aggregated.state = CfdState::Open;
            }
            CommitConfirmed => {
                // Commit can be published by either party, meaning it being confirmed might be the
                // first time we hear about it!
                self.aggregated.commit_published = true;

                self.aggregated.state = CfdState::OpenCommitted;
            }
            CetConfirmed => {
                self.aggregated.state = CfdState::Closed;
            }
            RefundConfirmed => {
                self.aggregated.state = CfdState::Refunded;
            }
            LockConfirmedAfterFinality | CollaborativeSettlementConfirmed => {
                self.aggregated.state = CfdState::Closed;
            }
            CetTimelockExpiredPriorOracleAttestation => {
                self.aggregated.state = CfdState::OpenCommitted;
            }
            CetTimelockExpiredPostOracleAttestation { cet } => {
                self.aggregated.cet = Some(cet);

                self.aggregated.state = CfdState::PendingCet;
            }
            RefundTimelockExpired { refund_tx } => {
                self.aggregated.refund_tx = Some(refund_tx);

                self.aggregated.refund_published = true;

                self.aggregated.state = CfdState::PendingRefund;
            }
            OracleAttestedPriorCetTimelock {
                timelocked_cet,
                price,
                ..
            } => {
                self.aggregated.timelocked_cet = Some(timelocked_cet);
                self.closing_price = Some(price);

                self.aggregated.commit_published = true;
                self.aggregated.state = CfdState::PendingCommit;
            }
            OracleAttestedPostCetTimelock { cet, price, .. } => {
                self.aggregated.cet = Some(cet);
                self.closing_price = Some(price);

                self.aggregated.state = CfdState::PendingCet;
            }
            ManualCommit { .. } => {
                self.aggregated.commit_published = true;

                self.aggregated.state = CfdState::PendingCommit;
            }
            RevokeConfirmed => {
                tracing::error!(order_id = %self.order_id, "Revoked logic not implemented");
                self.aggregated.state = CfdState::OpenCommitted;
            }
            RolloverStarted { .. } => {
                self.aggregated.rollover_state = Some(ProtocolNegotiationState::Started);
            }
            RolloverAccepted => {
                self.aggregated.rollover_state = Some(ProtocolNegotiationState::Accepted);
            }
        };

        self.state = self.aggregated.derive_cfd_state(self.role);
        self.actions = self.derive_actions();

        if let Some(lock_tx_url) = self.lock_tx_url(self.network) {
            self.details.tx_url_list.insert(lock_tx_url);
        }
        if let Some(commit_tx_url) = self.commit_tx_url(self.network) {
            self.details.tx_url_list.insert(commit_tx_url);
        }
        if let Some(collab_settlement_tx_url) = self.collab_settlement_tx_url(self.network) {
            self.details.tx_url_list.insert(collab_settlement_tx_url);
        }
        if let Some(refund_tx_url) = self.refund_tx_url(self.network) {
            self.details.tx_url_list.insert(refund_tx_url);
        }
        if let Some(cet_url) = self.cet_url(self.network) {
            self.details.tx_url_list.insert(cet_url);
        }

        self.aggregated.version += 1;

        self
    }

    pub fn with_current_quote(self, latest_quote: Option<xtra_bitmex_price_feed::Quote>) -> Self {
        // Closed CFDs should not be modified by the current quote
        if self.aggregated.state == CfdState::Closed {
            return self;
        }

        // If we have a dedicated closing price, use that one.
        if let Some(payout) = self.aggregated.clone().payout(self.role) {
            let payout = payout
                .to_signed()
                .expect("Amount to fit into signed amount");

            let (profit_btc, profit_percent) = calculate_profit(
                payout,
                self.margin
                    .to_signed()
                    .expect("Amount to fit into signed amount"),
            );

            return Self {
                payout: Some(payout),
                profit_btc: Some(profit_btc),
                profit_percent: Some(profit_percent.to_string()),
                ..self
            };
        }

        // Otherwise, compute based on current quote.
        let latest_quote = match latest_quote {
            Some(latest_quote) => latest_quote,
            None => {
                tracing::trace!(order_id = %self.order_id, "Unable to calculate profit/loss without current price");

                return Self {
                    payout: None,
                    profit_btc: None,
                    profit_percent: None,
                    ..self
                };
            }
        };

        let (bid, ask) = match (Price::new(latest_quote.bid), Price::new(latest_quote.ask)) {
            (Ok(bid), Ok(ask)) => (bid, ask),
            (Err(e), Err(_)) | (Err(e), Ok(_)) | (Ok(_), Err(e)) => {
                tracing::warn!(
                    "Failed to compute profit/loss because latest price is invalid: {e}"
                );

                return Self {
                    payout: None,
                    profit_btc: None,
                    profit_percent: None,
                    ..self
                };
            }
        };

        let closing_price = market_closing_price(bid, ask, self.role, self.position);

        let (long_leverage, short_leverage) =
            long_and_short_leverage(self.leverage_taker, self.role, self.position);

        let (profit_btc, profit_percent, payout) = match calculate_profit_at_price(
            self.initial_price,
            closing_price,
            self.quantity_usd,
            long_leverage,
            short_leverage,
            self.aggregated.fee_account,
        ) {
            Ok((profit_btc, profit_percent, payout)) => {
                (profit_btc, profit_percent.round_dp(1).to_string(), payout)
            }
            Err(e) => {
                tracing::warn!("Failed to calculate profit/loss {:#}", e);

                return Self {
                    payout: None,
                    profit_btc: None,
                    profit_percent: None,
                    ..self
                };
            }
        };

        Self {
            payout: Some(payout),
            profit_btc: Some(profit_btc),
            profit_percent: Some(profit_percent),
            ..self
        }
    }

    fn derive_actions(&self) -> HashSet<CfdAction> {
        match (self.state, self.role) {
            (CfdState::PendingSetup, Role::Maker) => {
                HashSet::from([CfdAction::AcceptOrder, CfdAction::RejectOrder])
            }
            (CfdState::PendingSetup, Role::Taker) => HashSet::new(),
            (CfdState::ContractSetup, _) => HashSet::new(),
            (CfdState::Rejected, _) => HashSet::new(),
            (CfdState::PendingOpen, _) => HashSet::new(),
            (CfdState::Open, _) => HashSet::from([CfdAction::Commit, CfdAction::Settle]),
            (CfdState::PendingCommit, _) => HashSet::new(),
            (CfdState::PendingCet, _) => HashSet::new(),
            (CfdState::PendingClose, _) => HashSet::new(),
            (CfdState::OpenCommitted, _) => HashSet::new(),
            (CfdState::IncomingSettlementProposal, Role::Maker) => {
                HashSet::from([CfdAction::AcceptSettlement, CfdAction::RejectSettlement])
            }
            (CfdState::IncomingSettlementProposal, Role::Taker) => HashSet::new(),
            (CfdState::OutgoingSettlementProposal, _) => HashSet::new(),
            (CfdState::IncomingRolloverProposal, Role::Maker) => {
                HashSet::from([CfdAction::AcceptRollover, CfdAction::RejectRollover])
            }
            (CfdState::IncomingRolloverProposal, Role::Taker) => HashSet::new(),
            (CfdState::OutgoingRolloverProposal, _) => HashSet::new(),
            (CfdState::RolloverSetup, _) => HashSet::new(),
            (CfdState::Closed, _) => HashSet::new(),
            (CfdState::PendingRefund, _) => HashSet::new(),
            (CfdState::Refunded, _) => HashSet::new(),
            (CfdState::SetupFailed, _) => HashSet::new(),
        }
    }

    /// Returns the URL to the lock transaction.
    ///
    /// If we have a DLC, we also have a lock transaction.
    fn lock_tx_url(&self, network: Network) -> Option<TxUrl> {
        let dlc = self.aggregated.latest_dlc.as_ref()?;
        let url = TxUrl::from_transaction(
            &dlc.lock.0,
            &dlc.lock.1.script_pubkey(),
            network,
            TxLabel::Lock,
        );

        Some(url)
    }

    fn commit_tx_url(&self, network: Network) -> Option<TxUrl> {
        if !self.aggregated.commit_published {
            return None;
        }

        let dlc = self.aggregated.latest_dlc.as_ref()?;
        let url = TxUrl::new(dlc.commit.0.txid(), network, TxLabel::Commit);

        Some(url)
    }

    fn collab_settlement_tx_url(&self, network: Network) -> Option<TxUrl> {
        let (tx, script) = self.aggregated.collab_settlement_tx.as_ref()?;
        let url = TxUrl::from_transaction(tx, script, network, TxLabel::Collaborative);

        Some(url)
    }

    fn refund_tx_url(&self, network: Network) -> Option<TxUrl> {
        if !self.aggregated.refund_published {
            return None;
        }

        let dlc = self.aggregated.latest_dlc.as_ref()?;

        let url = TxUrl::from_transaction(
            &dlc.refund.0,
            &dlc.script_pubkey_for(self.role),
            network,
            TxLabel::Refund,
        );

        Some(url)
    }

    fn cet_url(&self, network: Network) -> Option<TxUrl> {
        let tx = self.aggregated.cet.as_ref()?;
        let dlc = self.aggregated.latest_dlc.as_ref()?;

        let url =
            TxUrl::from_transaction(tx, &dlc.script_pubkey_for(self.role), network, TxLabel::Cet);

        Some(url)
    }
}

/// Internal struct to keep all the senders around in one place
struct Tx {
    cfds: watch::Sender<Option<Vec<Cfd>>>,
    pub order: watch::Sender<MakerOffers>,
    pub quote: watch::Sender<Option<Quote>>,
    // TODO: Use this channel to communicate maker status as well with generic
    // ID of connected counterparties
    pub connected_takers: watch::Sender<Vec<model::Identity>>,
}

impl Tx {
    fn send_cfds_update(
        &self,
        cfds: HashMap<OrderId, Cfd>,
        quote: Option<xtra_bitmex_price_feed::Quote>,
    ) {
        let cfds_with_quote = cfds
            .into_iter()
            .map(|(_, cfd)| cfd.with_current_quote(quote))
            .sorted_by(|a, b| {
                Ord::cmp(
                    &b.aggregated.creation_timestamp,
                    &a.aggregated.creation_timestamp,
                )
            })
            .collect();

        let _ = self.cfds.send(Some(cfds_with_quote));
    }

    fn send_quote_update(&self, quote: Option<xtra_bitmex_price_feed::Quote>) {
        let _ = self.quote.send(quote.map(|q| q.into()));
    }

    fn send_order_update(&self, offers: Option<model::MakerOffers>) {
        let (long, short) = match offers {
            None => (None, None),
            Some(offers) => {
                let projection_long =
                    offers
                        .long
                        .and_then(|long| match TryInto::<CfdOrder>::try_into(long) {
                            Ok(projection_long) => Some(projection_long),
                            Err(e) => {
                                tracing::warn!("Unable to convert long order: {e:#}");
                                None
                            }
                        });

                let projection_short =
                    offers
                        .short
                        .and_then(|short| match TryInto::<CfdOrder>::try_into(short) {
                            Ok(projection_short) => Some(projection_short),
                            Err(e) => {
                                tracing::warn!("Unable to convert short order: {e:#}");
                                None
                            }
                        });

                (projection_long, projection_short)
            }
        };

        let projection_offers = MakerOffers { long, short };

        let _ = self.order.send(projection_offers);
    }
}

/// Internal struct to keep state in one place
struct State {
    network: Network,
    quote: Option<xtra_bitmex_price_feed::Quote>,
    /// All hydrated CFDs.
    cfds: Option<HashMap<OrderId, Cfd>>,
}

impl db::CfdAggregate for Cfd {
    type CtorArgs = Network;

    fn new(args: Self::CtorArgs, cfd: db::Cfd) -> Self {
        Cfd::new(cfd, args)
    }

    fn apply(self, event: CfdEvent) -> Self {
        self.apply(event)
    }

    fn version(&self) -> u32 {
        self.aggregated.version
    }
}

impl db::ClosedCfdAggregate for Cfd {
    fn new_closed(network: Self::CtorArgs, closed_cfd: db::ClosedCfd) -> Self {
        let db::ClosedCfd {
            id,
            position,
            initial_price,
            taker_leverage,
            n_contracts,
            counterparty_network_identity,
            role,
            fees,
            expiry_timestamp,
            lock,
            settlement,
            creation_timestamp,
        } = closed_cfd;

        let quantity_usd = Usd::new(Decimal::from(u64::from(n_contracts)));

        let (our_leverage, counterparty_leverage) = match role {
            Role::Maker => (Leverage::ONE, taker_leverage),
            Role::Taker => (taker_leverage, Leverage::ONE),
        };

        let margin = calculate_margin(initial_price, quantity_usd, our_leverage);
        let margin_counterparty =
            calculate_margin(initial_price, quantity_usd, counterparty_leverage);

        let liquidation_price = match position {
            Position::Long => calculate_long_liquidation_price(our_leverage, initial_price),
            Position::Short => calculate_short_liquidation_price(our_leverage, initial_price),
        };

        let (details, closing_price, payout, state) = {
            let mut tx_url_list = HashSet::default();

            tx_url_list.insert(
                TxUrl::new(lock.txid.into(), network, TxLabel::Lock)
                    .with_output_index(lock.dlc_vout.into()),
            );

            let (price, payout, state) = match settlement {
                Settlement::Collaborative {
                    txid,
                    vout,
                    payout,
                    price,
                } => {
                    tx_url_list.insert(
                        TxUrl::new(txid.into(), network, TxLabel::Collaborative)
                            .with_output_index(vout.into()),
                    );
                    (Some(price), payout, CfdState::Closed)
                }
                Settlement::Cet {
                    commit_txid,
                    txid,
                    vout,
                    payout,
                    price,
                } => {
                    tx_url_list.insert(
                        TxUrl::new(commit_txid.into(), network, TxLabel::Commit)
                            .with_output_index(0),
                    );

                    tx_url_list.insert(
                        TxUrl::new(txid.into(), network, TxLabel::Cet)
                            .with_output_index(vout.into()),
                    );
                    (Some(price), payout, CfdState::Closed)
                }
                Settlement::Refund {
                    commit_txid,
                    txid,
                    vout,
                    payout,
                } => {
                    tx_url_list.insert(
                        TxUrl::new(commit_txid.into(), network, TxLabel::Commit)
                            .with_output_index(0),
                    );

                    tx_url_list.insert(
                        TxUrl::new(txid.into(), network, TxLabel::Refund)
                            .with_output_index(vout.into()),
                    );
                    (None, payout, CfdState::Refunded)
                }
            };

            (
                CfdDetails { tx_url_list },
                price,
                SignedAmount::from(payout),
                state,
            )
        };

        let (profit_btc, profit_percent) = calculate_profit(
            payout,
            margin
                .to_signed()
                .expect("Amount to fit into signed amount"),
        );

        // there are no events to apply at this stage for closed CFDs,
        // which is why this field is mostly ignored
        let mut aggregated = Aggregated::new(FeeAccount::new(position, role));

        // set the creation_timestamp to be able to sort closed CFDs
        aggregated.creation_timestamp = creation_timestamp;

        Self {
            order_id: id,
            initial_price,
            accumulated_fees: fees.into(),
            leverage_taker: taker_leverage,
            trading_pair: TradingPair::BtcUsd,
            position,
            liquidation_price,
            quantity_usd,
            margin,
            margin_counterparty,
            role,

            profit_btc: Some(profit_btc),
            profit_percent: Some(profit_percent.to_string()),
            payout: Some(payout),
            closing_price,

            state,
            actions: HashSet::default(),
            details,
            expiry_timestamp: Some(expiry_timestamp),
            counterparty: counterparty_network_identity,
            pending_settlement_proposal_price: None,
            aggregated,
            network,
        }
    }
}

impl db::FailedCfdAggregate for Cfd {
    fn new_failed(network: Self::CtorArgs, failed_cfd: db::FailedCfd) -> Self {
        let db::FailedCfd {
            id,
            position,
            initial_price,
            taker_leverage,
            n_contracts,
            counterparty_network_identity,
            role,
            fees,
            kind,
            creation_timestamp,
        } = failed_cfd;

        let state = match kind {
            db::Kind::OfferRejected => CfdState::Rejected,
            db::Kind::ContractSetupFailed => CfdState::SetupFailed,
        };

        let quantity_usd =
            Usd::new(Decimal::from_u64(u64::from(n_contracts)).expect("u64 to fit into Decimal"));

        let (our_leverage, counterparty_leverage) = match role {
            Role::Maker => (Leverage::ONE, taker_leverage),
            Role::Taker => (taker_leverage, Leverage::ONE),
        };

        let margin = calculate_margin(initial_price, quantity_usd, our_leverage);
        let margin_counterparty =
            calculate_margin(initial_price, quantity_usd, counterparty_leverage);

        let liquidation_price = match position {
            Position::Long => calculate_long_liquidation_price(our_leverage, initial_price),
            Position::Short => calculate_short_liquidation_price(our_leverage, initial_price),
        };

        // there are no events to apply at this stage for failed CFDs,
        // which is why this field is mostly ignored
        let mut aggregated = Aggregated::new(FeeAccount::new(position, role));

        // set the creation_timestamp to be able to sort failed CFDs
        aggregated.creation_timestamp = creation_timestamp;

        Self {
            order_id: id,
            initial_price,
            accumulated_fees: fees.into(),
            leverage_taker: taker_leverage,
            trading_pair: TradingPair::BtcUsd,
            position,
            liquidation_price,
            quantity_usd,
            margin,
            margin_counterparty,
            role,

            profit_btc: None,
            profit_percent: None,
            payout: None,
            closing_price: None,

            state,
            actions: HashSet::default(),
            details: CfdDetails {
                tx_url_list: HashSet::default(),
            },
            expiry_timestamp: None,
            counterparty: counterparty_network_identity,
            pending_settlement_proposal_price: None,
            aggregated,
            network,
        }
    }
}

impl State {
    fn new(network: Network) -> Self {
        Self {
            network,
            quote: None,
            cfds: None,
        }
    }

    async fn update_cfd(&mut self, db: db::Connection, id: OrderId) -> Result<()> {
        let cfd = db.load_open_cfd(id, self.network).await?;

        let cfds = self
            .cfds
            .as_mut()
            .context("CFD list has not been initialized yet")?;

        cfds.insert(id, cfd);

        Ok(())
    }

    fn update_quote(&mut self, quote: Option<xtra_bitmex_price_feed::Quote>) {
        self.quote = quote;
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: Initialize) -> Result<()> {
        let mut stream = self.db.load_all_cfds::<Cfd>(self.state.network);

        let mut cfds = HashMap::new();

        while let Some(cfd) = stream.next().await {
            let cfd = match cfd {
                Ok(cfd) => cfd,
                Err(e) => {
                    tracing::error!("Failed to rehydrate CFD: {e:#}");
                    continue;
                }
            };

            cfds.insert(cfd.order_id, cfd);
        }

        self.state.cfds = Some(cfds);

        self.tx.send_cfds_update(
            self.state
                .cfds
                .clone()
                .expect("we initialized the state above; qed"),
            self.state.quote,
        );

        Ok(())
    }

    async fn handle(&mut self, msg: CfdChanged) {
        if let Err(e) = self.state.update_cfd(self.db.clone(), msg.0).await {
            tracing::error!("Failed to rehydrate CFD: {e:#}");
            return;
        };

        self.tx.send_cfds_update(
            self.state
                .cfds
                .clone()
                .expect("update_cfd fails if the CFDs have not been initialized yet"),
            self.state.quote,
        );
    }

    fn handle(&mut self, msg: Update<Option<model::MakerOffers>>) {
        self.tx.send_order_update(msg.0);
    }

    fn handle(&mut self, msg: Update<Option<xtra_bitmex_price_feed::Quote>>) {
        self.state.update_quote(msg.0);
        self.tx.send_quote_update(msg.0);

        let hydrated_cfds = match self.state.cfds.clone() {
            None => {
                tracing::debug!("Cannot update CFDs with new quote until they are initialized.");
                return;
            }
            Some(cfds) => cfds,
        };

        self.tx.send_cfds_update(hydrated_cfds, msg.0);
    }

    fn handle(&mut self, msg: Update<Vec<model::Identity>>) {
        let _ = self.tx.connected_takers.send(msg.0);
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we just started");
        this.send_async_safe(Initialize)
            .await
            .expect("we just started");

        self.tasks.add({
            let price_feed = self.price_feed.clone_channel();

            async move {
                loop {
                    match price_feed.send(xtra_bitmex_price_feed::LatestQuote).await {
                        Ok(quote) => {
                            let _ = this.send(Update(quote)).await;
                        }
                        Err(_) => {
                            tracing::trace!("Price feed actor currently unreachable");
                        }
                    }

                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        })
    }

    async fn stopped(self) -> Self::Stop {}
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Quote {
    #[serde(with = "round_to_two_dp")]
    bid: Decimal,
    #[serde(with = "round_to_two_dp")]
    ask: Decimal,
    last_updated_at: Timestamp,
}

impl From<xtra_bitmex_price_feed::Quote> for Quote {
    fn from(quote: xtra_bitmex_price_feed::Quote) -> Self {
        Quote {
            bid: quote.bid,
            ask: quote.ask,
            last_updated_at: Timestamp::new(quote.timestamp.unix_timestamp()),
        }
    }
}

/// Maker offers represents the offers as cerated by the maker
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MakerOffers {
    /// The offer where the maker's position is long
    pub long: Option<CfdOrder>,
    /// The offer where the maker's position is short
    pub short: Option<CfdOrder>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CfdOrder {
    pub id: OrderId,

    pub trading_pair: TradingPair,

    #[serde(rename = "position")]
    pub position_maker: Position,

    /// The maker's price for opening a position
    #[serde(with = "round_to_two_dp")]
    pub price: Price,

    /// Fee charged by the maker for opening a position
    ///
    /// Note: It's a flat fee on top of the fee calculated based on funding rate
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc::opt")]
    pub opening_fee: Option<Amount>,

    /// The interest as annualized percentage
    ///
    /// This is an estimate as the interest rate can fluctuate.
    /// If it is positive it means the taker receives, if it is negative it means the taker pays.
    pub interest_rate_annualized_percent: String,

    /// The current interest rate by the hour
    ///
    /// This represents the current interest rate asked/offered by the maker.
    /// The interest rate fluctuates with market movements.
    /// If it is positive it means the taker receives, if it is negative it means the taker pays.
    pub interest_rate_hourly_percent: String,

    #[serde(with = "round_to_two_dp")]
    pub min_quantity: Usd,
    #[serde(with = "round_to_two_dp")]
    pub max_quantity: Usd,

    /// The user can only buy contracts in multiples of this.
    ///
    /// For example, if `lot_size` is 100, `min_quantity` is 300 and `max_quantity`is 800, then
    /// the user can buy 300, 400, 500, 600, 700 or 800 contracts.
    #[serde(with = "round_to_two_dp")]
    pub lot_size: Usd,

    #[serde(rename = "leverage")]
    pub taker_leverage_choices: Leverage,

    /// Own liquidation price according to position and leverage
    #[serde(with = "round_to_two_dp")]
    pub liquidation_price: Price,

    pub creation_timestamp: Timestamp,
    pub settlement_time_interval_in_secs: u64,

    /// Margin per lot from the perspective of the role
    ///
    /// Since this is a calculated value that we need in the UI this value is based on the
    /// perspective the role (i.e. taker/maker)
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub margin_per_lot: Amount,

    /// Initial funding fee per lot from the perspective of the role
    ///
    /// Since this is a calculated value that we need in the UI this value is based on the
    /// perspective the role (i.e. taker/maker)
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub initial_funding_fee_per_lot: SignedAmount,
}

impl TryFrom<Order> for CfdOrder {
    type Error = anyhow::Error;

    fn try_from(order: Order) -> std::result::Result<Self, Self::Error> {
        let lot_size = Usd::new(dec!(100)); // TODO: Have the maker tell us this.

        let role = order.origin.into();
        let own_position = match order.origin {
            // we are the maker, the order's position is our position
            Origin::Ours => order.position_maker,
            // we are the taker, the order's position is our counter-position
            Origin::Theirs => order.position_maker.counter_position(),
        };

        let (long_leverage, short_leverage) =
            long_and_short_leverage(order.leverage_taker, role, own_position);

        let initial_funding_fee_per_lot = FundingFee::calculate(
            order.price,
            lot_size,
            long_leverage,
            short_leverage,
            order.funding_rate,
            SETTLEMENT_INTERVAL.whole_hours(),
        )
        .context("unable to calculate initial funding fee")?;

        let interest_rate =
            InterestRate::from((order.funding_rate, order.position_maker.counter_position()));

        // Use a temporary fee account to define the funding fee's sign
        let temp_fee_account = FeeAccount::new(own_position, role);
        let initial_funding_fee_per_lot = temp_fee_account
            .add_funding_fee(initial_funding_fee_per_lot)
            .balance();

        // Liquidation price is dependent on one's own leverage
        let liquidation_price = match own_position {
            Position::Long => calculate_long_liquidation_price(long_leverage, order.price),
            Position::Short => calculate_short_liquidation_price(short_leverage, order.price),
        };

        // Margin per lot price is dependent on one's own leverage
        let margin_per_lot = match own_position {
            Position::Long => calculate_margin(order.price, lot_size, long_leverage),
            Position::Short => calculate_margin(order.price, lot_size, short_leverage),
        };

        Ok(Self {
            id: order.id,
            trading_pair: order.trading_pair,
            position_maker: order.position_maker,
            price: order.price,
            min_quantity: order.min_quantity,
            max_quantity: order.max_quantity,
            lot_size,
            margin_per_lot,
            taker_leverage_choices: order.leverage_taker,
            liquidation_price,
            creation_timestamp: order.creation_timestamp_maker,
            settlement_time_interval_in_secs: order
                .settlement_interval
                .whole_seconds()
                .try_into()
                .context("unable to convert settlement interval")?,
            opening_fee: Some(order.opening_fee.to_inner()),
            interest_rate_annualized_percent: AnnualisedInterestRatePercent::from(interest_rate)
                .to_string(),
            interest_rate_hourly_percent: HourlyInterestRatePercent::from(interest_rate)
                .to_string(),
            initial_funding_fee_per_lot,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum CfdState {
    PendingSetup,
    ContractSetup,
    Rejected,
    PendingOpen,
    Open,
    PendingCommit,
    PendingCet,
    PendingClose,
    OpenCommitted,
    IncomingSettlementProposal,
    OutgoingSettlementProposal,
    IncomingRolloverProposal,
    OutgoingRolloverProposal,
    RolloverSetup,
    Closed,
    PendingRefund,
    Refunded,
    SetupFailed,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CfdDetails {
    tx_url_list: HashSet<TxUrl>,
}

#[derive(Debug, Clone, Copy, Display, FromStr, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
#[display(style = "camelCase")]
pub enum CfdAction {
    AcceptOrder,
    RejectOrder,
    Commit,
    Settle,
    AcceptSettlement,
    RejectSettlement,
    AcceptRollover,
    RejectRollover,
}

mod round_to_two_dp {
    use super::*;
    use serde::Serializer;

    pub trait ToDecimal {
        fn to_decimal(&self) -> Decimal;
    }

    impl ToDecimal for Usd {
        fn to_decimal(&self) -> Decimal {
            self.into_decimal()
        }
    }

    impl ToDecimal for Price {
        fn to_decimal(&self) -> Decimal {
            self.into_decimal()
        }
    }

    impl ToDecimal for Decimal {
        fn to_decimal(&self) -> Decimal {
            *self
        }
    }

    pub fn serialize<D: ToDecimal, S: Serializer>(
        value: &D,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let decimal = value.to_decimal();
        let decimal = decimal.round_dp(2);

        Serialize::serialize(&decimal, serializer)
    }

    pub mod opt {
        use super::*;

        pub fn serialize<D: ToDecimal, S: Serializer>(
            value: &Option<D>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            match value {
                None => serializer.serialize_none(),
                Some(value) => super::serialize(value, serializer),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use rust_decimal_macros::dec;
        use serde_test::assert_ser_tokens;
        use serde_test::Token;

        #[derive(Serialize)]
        #[serde(transparent)]
        struct WithOnlyTwoDecimalPlaces<I: ToDecimal> {
            #[serde(with = "super")]
            inner: I,
        }

        #[test]
        fn usd_serializes_with_only_cents() {
            let usd = WithOnlyTwoDecimalPlaces {
                inner: model::Usd::new(dec!(1000.12345)),
            };

            assert_ser_tokens(&usd, &[Token::Str("1000.12")]);
        }

        #[test]
        fn price_serializes_with_only_cents() {
            let price = WithOnlyTwoDecimalPlaces {
                inner: model::Price::new(dec!(1000.12345)).unwrap(),
            };

            assert_ser_tokens(&price, &[Token::Str("1000.12")]);
        }
    }
}

/// Construct a mempool.space URL for a given txid
pub fn to_mempool_url(txid: Txid, network: Network) -> String {
    match network {
        Network::Bitcoin => format!("https://mempool.space/tx/{txid}"),
        Network::Testnet => format!("https://mempool.space/testnet/tx/{txid}"),
        Network::Signet => format!("https://mempool.space/signet/tx/{txid}"),
        Network::Regtest => txid.to_string(),
    }
}

/// Link to transaction on mempool.space for UI representation
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash)]
struct TxUrl {
    pub label: TxLabel,
    pub url: String,
}

impl TxUrl {
    fn new(txid: Txid, network: Network, label: TxLabel) -> Self {
        Self {
            label,
            url: to_mempool_url(txid, network),
        }
    }

    /// Highlight particular transaction output in the TxUrl
    fn with_output_index(mut self, index: u32) -> Self {
        self.url.push_str(&format!(":{index}"));
        self
    }

    /// If the Transaction contains the script_pubkey, output will be selected
    /// in the URL. Otherwise, fall back to the main txid URL.
    fn from_transaction(
        transaction: &Transaction,
        script_pubkey: &Script,
        network: Network,
        label: TxLabel,
    ) -> Self {
        debug_assert!(label != TxLabel::Commit, "commit transaction has a single output which does not belong to either party - this won't highlight anything");
        let tx_url = Self::new(transaction.txid(), network, label);
        if let Ok(outpoint) = transaction.outpoint(script_pubkey) {
            tx_url.with_output_index(outpoint.vout)
        } else {
            tx_url
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Eq, Hash)]
pub enum TxLabel {
    Lock,
    Commit,
    Cet,
    Refund,
    Collaborative,
}

/// The interest as annualized percentage
///
/// This is an estimate as the interest rate can fluctuate.
/// Similar to a bank account: If it is positive it means the taker receives, if it is negative it
/// means the taker pays.
struct AnnualisedInterestRatePercent(Decimal);

impl From<InterestRate> for AnnualisedInterestRatePercent {
    fn from(interest_rate: InterestRate) -> Self {
        let interest_rate_annualized = interest_rate
            .to_decimal()
            .checked_mul(dec!(100))
            .expect("Not to overflow for funding rate")
            .checked_mul(Decimal::from(
                (24 / SETTLEMENT_INTERVAL.whole_hours()) * 365,
            ))
            .expect("not to overflow");
        Self(interest_rate_annualized)
    }
}

impl fmt::Display for AnnualisedInterestRatePercent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.round_dp(2).fmt(f)
    }
}

/// The interest rate by the hour as percentage
///
/// This represents the current interest rate asked/offered by the maker.
/// Similar to a bank account: If it is positive it means the taker receives, if it is negative it
/// means the taker pays.
struct HourlyInterestRatePercent(Decimal);

impl From<InterestRate> for HourlyInterestRatePercent {
    fn from(interest_rate: InterestRate) -> Self {
        let interest_rate_hourly = interest_rate
            .to_decimal()
            .checked_mul(dec!(100))
            .expect("Not to overflow for funding rate")
            .checked_div(Decimal::from(SETTLEMENT_INTERVAL.whole_hours()))
            .expect("Not to fail as funding rate is sanitised");
        Self(interest_rate_hourly)
    }
}

#[derive(Clone, Copy)]
struct InterestRate(Decimal);

impl InterestRate {
    pub fn to_decimal(self) -> Decimal {
        self.0
    }
}

/// Converts the funding rate into interest rate from the perspective of {position}
///
/// We take into account the position and the funding rate:
/// - A positive funding rate means "long pay short"
/// - A negative funding rate means "short pay long'
/// Similar to a bank account:
/// - If the interest rate is positive it means the taker receives,
/// - If the interest rate is negative it means the taker pays.
impl From<(FundingRate, Position)> for InterestRate {
    fn from((funding_rate, position): (FundingRate, Position)) -> Self {
        match (position, funding_rate.to_decimal().is_sign_positive()) {
            (Position::Long, true) | (Position::Short, false) => {
                InterestRate(funding_rate.to_decimal().neg())
            }
            (Position::Long, false) | (Position::Short, true) => {
                InterestRate(funding_rate.to_decimal())
            }
        }
    }
}

impl fmt::Display for HourlyInterestRatePercent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_snapshot_test() {
        // Make sure to update the UI after changing this test!

        let json = serde_json::to_string(&CfdState::PendingSetup).unwrap();
        assert_eq!(json, "\"PendingSetup\"");
        let json = serde_json::to_string(&CfdState::ContractSetup).unwrap();
        assert_eq!(json, "\"ContractSetup\"");
        let json = serde_json::to_string(&CfdState::Rejected).unwrap();
        assert_eq!(json, "\"Rejected\"");
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
    fn funding_rate_positive_and_you_are_short_then_interest_positive_meaning_you_receive() {
        let funding_rate = FundingRate::new(dec!(1.0)).expect("To be valid funding rate");
        let your_position = Position::Short;

        let interest_rate = InterestRate::from((funding_rate, your_position));
        let hourly = HourlyInterestRatePercent::from(interest_rate);
        let annually = AnnualisedInterestRatePercent::from(interest_rate);

        assert!(interest_rate.0.is_sign_positive());
        assert!(hourly.0.is_sign_positive());
        assert!(annually.0.is_sign_positive());
    }

    #[test]
    fn funding_rate_positive_and_you_are_long_then_interest_negative_meaning_you_pay() {
        let funding_rate = FundingRate::new(dec!(1.0)).expect("To be valid funding rate");
        let your_position = Position::Long;

        let interest_rate = InterestRate::from((funding_rate, your_position));
        let hourly = HourlyInterestRatePercent::from(interest_rate);
        let annually = AnnualisedInterestRatePercent::from(interest_rate);

        assert!(interest_rate.0.is_sign_negative());
        assert!(hourly.0.is_sign_negative());
        assert!(annually.0.is_sign_negative());
    }

    #[test]
    fn funding_rate_negative_and_you_are_short_then_interest_negative_meaning_you_receive() {
        let funding_rate = FundingRate::new(dec!(-1.0)).expect("To be valid funding rate");
        let your_position = Position::Short;

        let interest_rate = InterestRate::from((funding_rate, your_position));
        let hourly = HourlyInterestRatePercent::from(interest_rate);
        let annually = AnnualisedInterestRatePercent::from(interest_rate);

        assert!(interest_rate.0.is_sign_positive());
        assert!(hourly.0.is_sign_positive());
        assert!(annually.0.is_sign_positive());
    }

    #[test]
    fn funding_rate_negative_and_you_are_long_then_interest_positive_meaning_you_pay() {
        let funding_rate = FundingRate::new(dec!(-1.0)).expect("To be valid funding rate");
        let your_position = Position::Long;

        let interest_rate = InterestRate::from((funding_rate, your_position));
        let hourly = HourlyInterestRatePercent::from(interest_rate);
        let annually = AnnualisedInterestRatePercent::from(interest_rate);

        assert!(interest_rate.0.is_sign_negative());
        assert!(hourly.0.is_sign_negative());
        assert!(annually.0.is_sign_negative());
    }
}
