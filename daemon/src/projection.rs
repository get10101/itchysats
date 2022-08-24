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
use maia_core::TransactionExt;
use model::calculate_long_liquidation_price;
use model::calculate_margin;
use model::calculate_payout_at_price;
use model::calculate_profit;
use model::calculate_short_liquidation_price;
use model::long_and_short_leverage;
use model::market_closing_price;
use model::CfdEvent;
use model::ClosedCfd;
use model::ContractSymbol;
use model::Contracts;
use model::Dlc;
use model::EventKind;
use model::FailedCfd;
use model::FailedKind;
use model::FeeAccount;
use model::FundingFee;
use model::FundingRate;
use model::Leverage;
use model::LotSize;
use model::OfferId;
use model::OrderId;
use model::Position;
use model::Price;
use model::Role;
use model::Settlement;
use model::Timestamp;
use model::SETTLEMENT_INTERVAL;
use parse_display::Display;
use parse_display::FromStr;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use serde::Serialize;
use sqlite_db;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Write;
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::sync::watch;
use tracing::info_span;
use tracing::Instrument;
use xtra::prelude::MessageChannel;
use xtra_bitmex_price_feed::GetLatestQuotes;
use xtra_productivity::xtra_productivity;
use xtras::SendAsyncNext;

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
    db: sqlite_db::Connection,
    tx: Tx,
    state: State,
    price_feed: MessageChannel<GetLatestQuotes, xtra_bitmex_price_feed::LatestQuotes>,
    role: Role,
}

pub struct FeedReceivers {
    pub quote: watch::Receiver<LatestQuotes>,
    pub offers: watch::Receiver<MakerOffers>,
    pub cfds: watch::Receiver<Option<Vec<Cfd>>>,
}

pub struct FeedSenders {
    pub quote: watch::Sender<LatestQuotes>,
    pub offers: watch::Sender<MakerOffers>,
    pub cfds: watch::Sender<Option<Vec<Cfd>>>,
}

pub fn feeds() -> (FeedSenders, FeedReceivers) {
    let (tx_quote, rx_quote) = watch::channel(LatestQuotes::default());
    let (tx_offers, rx_offers) = watch::channel(MakerOffers::default());
    let (tx_cfds, rx_cfds) = watch::channel(None);

    (
        FeedSenders {
            quote: tx_quote,
            offers: tx_offers,
            cfds: tx_cfds,
        },
        FeedReceivers {
            quote: rx_quote,
            offers: rx_offers,
            cfds: rx_cfds,
        },
    )
}

impl Actor {
    pub fn new(
        db: sqlite_db::Connection,
        network: Network,
        price_feed: MessageChannel<GetLatestQuotes, xtra_bitmex_price_feed::LatestQuotes>,
        role: Role,
        feed_senders: Arc<FeedSenders>,
    ) -> Self {
        Self {
            db,
            tx: Tx(feed_senders),
            state: State::new(network),
            price_feed,
            role,
        }
    }
}

#[derive(Derivative, Clone, Debug, Serialize)]
#[derivative(PartialEq)]
pub struct Cfd {
    pub order_id: OrderId,
    pub offer_id: OfferId,
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
    pub contract_symbol: ContractSymbol,
    pub position: Position,
    #[serde(with = "round_to_two_dp")]
    pub liquidation_price: Price,

    #[serde(with = "round_to_two_dp")]
    pub quantity: Contracts,

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

    /// Projected or final payout
    ///
    /// If we don't know the final payout yet then we calculate this based on the projected profit.
    /// If we don't have a current price in this scenario we don't know the payout, hence it is
    /// represented as option. If we already know the final payout (based on CET or
    /// collborative close) then this is the final payout.
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc::opt")]
    pub payout: Option<Amount>,
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
pub struct Aggregated {
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

    /// Negotiation state of collaborative settlement protocol.
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
        self.state
    }

    // Only used in integration tests
    pub fn latest_dlc(&self) -> &Option<Dlc> {
        &self.latest_dlc
    }
    // Only used in integration tests
    pub fn latest_fees(&self) -> model::CompleteFee {
        self.fee_account.settle()
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
        sqlite_db::Cfd {
            id,
            offer_id,
            position,
            initial_price,
            taker_leverage,
            quantity,
            counterparty_network_identity,
            role,
            opening_fee,
            initial_funding_rate,
            contract_symbol,
            ..
        }: sqlite_db::Cfd,
        network: Network,
    ) -> Self {
        let (our_leverage, counterparty_leverage) = match role {
            Role::Maker => (Leverage::ONE, taker_leverage),
            Role::Taker => (taker_leverage, Leverage::ONE),
        };

        let margin = calculate_margin(contract_symbol, initial_price, quantity, our_leverage);
        let margin_counterparty = calculate_margin(
            contract_symbol,
            initial_price,
            quantity,
            counterparty_leverage,
        );

        let liquidation_price = match position {
            Position::Long => calculate_long_liquidation_price(
                initial_price,
                quantity_usd,
                our_leverage,
                contract_symbol,
            ),
            Position::Short => calculate_short_liquidation_price(
                initial_price,
                quantity_usd,
                our_leverage,
                contract_symbol,
            ),
        };

        let (long_leverage, short_leverage) =
            long_and_short_leverage(taker_leverage, role, position);

        let initial_funding_fee = FundingFee::calculate(
            initial_price,
            quantity,
            long_leverage,
            short_leverage,
            initial_funding_rate,
            SETTLEMENT_INTERVAL.whole_hours(),
            contract_symbol,
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
            offer_id,
            initial_price,
            accumulated_fees: fee_account.balance(),
            leverage_taker: taker_leverage,
            contract_symbol,
            position,
            liquidation_price,
            quantity,
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
                self.expiry_timestamp = dlc.as_ref().map(|dlc| dlc.settlement_event_id.timestamp());
                self.aggregated.latest_dlc = dlc;

                self.aggregated.state = CfdState::PendingOpen;
            }
            ContractSetupFailed => {
                self.aggregated.state = CfdState::SetupFailed;
            }
            OfferRejected => {
                self.aggregated.state = CfdState::Rejected;
            }
            RolloverCompleted {
                dlc,
                funding_fee,
                complete_fee,
            } => {
                self.expiry_timestamp = dlc.as_ref().map(|dlc| dlc.settlement_event_id.timestamp());
                self.aggregated.latest_dlc = dlc;

                self.aggregated.fee_account = match complete_fee {
                    None => self.aggregated.fee_account.add_funding_fee(funding_fee),
                    Some(complete_fee) => {
                        self.aggregated.fee_account.from_complete_fee(complete_fee)
                    }
                };

                self.accumulated_fees = self.aggregated.fee_account.balance();

                self.aggregated.state = CfdState::Open;
            }
            RolloverAccepted | RolloverStarted { .. } => {
                self.aggregated.state = CfdState::RolloverSetup;
            }
            RolloverRejected | RolloverFailed => {
                self.aggregated.state = CfdState::Open;
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
                // Needed for cases where CET gets priority over refund in case the refund timelock
                // is already expired. If the CET is confirmed we have to ensure the
                // refund-tx is not set, otherwise the UI will show the refund-tx.
                self.aggregated.refund_tx = None;
                self.aggregated.refund_published = false;

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
                // TODO: Implement revoked logic
                self.aggregated.state = CfdState::OpenCommitted;
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

    pub fn with_current_quote(self, latest_quotes: Option<&LatestQuotes>) -> Self {
        // If the payout was already set we don't care about the current quote, this applies to
        // closed CFDs
        if self.payout.is_some() {
            return self;
        }

        // If we have a dedicated closing price, use that one.
        if let Some(payout) = self.aggregated.clone().payout(self.role) {
            let (profit_btc, profit_percent) = calculate_profit(payout, self.margin);

            return Self {
                payout: Some(payout),
                profit_btc: Some(profit_btc),
                profit_percent: Some(profit_percent.to_string()),
                ..self
            };
        }

        // Otherwise, compute based on current quote.
        let latest_quote = latest_quotes.and_then(|quote| quote.get(&self.contract_symbol));

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

        let (profit_btc, profit_percent, payout) = match calculate_payout_at_price(
            self.contract_symbol,
            self.initial_price,
            closing_price,
            self.quantity,
            long_leverage,
            short_leverage,
            self.aggregated.fee_account,
        ) {
            Ok(payout) => {
                let (profit_btc, profit_percent) = calculate_profit(payout, self.margin);

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
            &dlc.lock.0.clone(),
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

    // Only used in integration tests
    pub fn aggregated(&self) -> &Aggregated {
        &self.aggregated
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
            &dlc.refund.0.clone(),
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
struct Tx(Arc<FeedSenders>);

impl Tx {
    fn send_cfds_update(&self, cfds: HashMap<OrderId, Cfd>, quotes: &LatestQuotes) {
        let cfds_with_quote = cfds
            .into_iter()
            .map(|(_, cfd)| cfd.with_current_quote(Some(quotes)))
            .sorted_by(|a, b| {
                Ord::cmp(
                    &b.aggregated.creation_timestamp,
                    &a.aggregated.creation_timestamp,
                )
            })
            .collect();

        let _ = self.0.cfds.send(Some(cfds_with_quote));
    }

    fn send_quotes_update(&self, quotes: LatestQuotes) {
        let _ = self.0.quote.send(quotes);
    }

    fn send_offer_update(&self, offers: MakerOffers) -> Result<()> {
        self.0.offers.send(offers)?;

        Ok(())
    }
}

/// Internal struct to keep state in one place
struct State {
    network: Network,
    latest_quotes: LatestQuotes,
    offers: MakerOffers,
    /// All hydrated CFDs.
    cfds: Option<HashMap<OrderId, Cfd>>,
}

impl sqlite_db::CfdAggregate for Cfd {
    type CtorArgs = Network;

    fn new(args: Self::CtorArgs, cfd: sqlite_db::Cfd) -> Self {
        Cfd::new(cfd, args)
    }

    fn apply(self, event: CfdEvent) -> Self {
        self.apply(event)
    }

    fn version(&self) -> u32 {
        self.aggregated.version
    }
}

impl sqlite_db::ClosedCfdAggregate for Cfd {
    fn new_closed(network: Self::CtorArgs, closed_cfd: ClosedCfd) -> Self {
        let ClosedCfd {
            id,
            offer_id,
            position,
            initial_price,
            taker_leverage,
            n_contracts: quantity,
            counterparty_network_identity,
            role,
            fees,
            expiry_timestamp,
            lock,
            settlement,
            creation_timestamp,
            contract_symbol,
            ..
        } = closed_cfd;

        let (our_leverage, counterparty_leverage) = match role {
            Role::Maker => (Leverage::ONE, taker_leverage),
            Role::Taker => (taker_leverage, Leverage::ONE),
        };

        let margin = calculate_margin(contract_symbol, initial_price, quantity, our_leverage);
        let margin_counterparty = calculate_margin(
            contract_symbol,
            initial_price,
            quantity,
            counterparty_leverage,
        );

        let liquidation_price = match position {
            Position::Long => calculate_long_liquidation_price(
                initial_price,
                quantity_usd,
                our_leverage,
                contract_symbol,
            ),
            Position::Short => calculate_short_liquidation_price(
                initial_price,
                quantity_usd,
                our_leverage,
                contract_symbol,
            ),
        };

        let (details, closing_price, payout, state) = {
            let mut tx_url_list = HashSet::default();

            tx_url_list.insert(
                TxUrl::new(lock.txid, network, TxLabel::Lock)
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
                        TxUrl::new(txid, network, TxLabel::Collaborative)
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
                        TxUrl::new(commit_txid, network, TxLabel::Commit).with_output_index(0),
                    );

                    tx_url_list.insert(
                        TxUrl::new(txid, network, TxLabel::Cet).with_output_index(vout.into()),
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
                        TxUrl::new(commit_txid, network, TxLabel::Commit).with_output_index(0),
                    );

                    tx_url_list.insert(
                        TxUrl::new(txid, network, TxLabel::Refund).with_output_index(vout.into()),
                    );
                    (None, payout, CfdState::Refunded)
                }
            };

            (CfdDetails { tx_url_list }, price, payout, state)
        };

        let (profit_btc, profit_percent) = calculate_profit(payout.inner(), margin);

        // there are no events to apply at this stage for closed CFDs,
        // which is why this field is mostly ignored
        let mut aggregated = Aggregated::new(FeeAccount::new(position, role));

        // set the creation_timestamp to be able to sort closed CFDs
        aggregated.creation_timestamp = creation_timestamp;

        Self {
            order_id: id,
            offer_id,
            initial_price,
            accumulated_fees: fees.into(),
            leverage_taker: taker_leverage,
            contract_symbol,
            position,
            liquidation_price,
            quantity,
            margin,
            margin_counterparty,
            role,

            profit_btc: Some(profit_btc),
            profit_percent: Some(profit_percent.to_string()),
            payout: Some(payout.inner()),
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

impl sqlite_db::FailedCfdAggregate for Cfd {
    fn new_failed(network: Self::CtorArgs, failed_cfd: FailedCfd) -> Self {
        let FailedCfd {
            id,
            offer_id,
            position,
            initial_price,
            taker_leverage,
            n_contracts: quantity,
            counterparty_network_identity,
            role,
            fees,
            kind,
            creation_timestamp,
            contract_symbol,
            ..
        } = failed_cfd;

        let state = match kind {
            FailedKind::OfferRejected => CfdState::Rejected,
            FailedKind::ContractSetupFailed => CfdState::SetupFailed,
        };

        let (our_leverage, counterparty_leverage) = match role {
            Role::Maker => (Leverage::ONE, taker_leverage),
            Role::Taker => (taker_leverage, Leverage::ONE),
        };

        let margin = calculate_margin(contract_symbol, initial_price, quantity, our_leverage);
        let margin_counterparty = calculate_margin(
            contract_symbol,
            initial_price,
            quantity,
            counterparty_leverage,
        );

        let liquidation_price = match position {
            Position::Long => calculate_long_liquidation_price(
                initial_price,
                quantity_usd,
                our_leverage,
                contract_symbol,
            ),
            Position::Short => calculate_short_liquidation_price(
                initial_price,
                quantity_usd,
                our_leverage,
                contract_symbol,
            ),
        };

        // there are no events to apply at this stage for failed CFDs,
        // which is why this field is mostly ignored
        let mut aggregated = Aggregated::new(FeeAccount::new(position, role));

        // set the creation_timestamp to be able to sort failed CFDs
        aggregated.creation_timestamp = creation_timestamp;

        Self {
            order_id: id,
            offer_id,
            initial_price,
            accumulated_fees: fees.into(),
            leverage_taker: taker_leverage,
            contract_symbol,
            position,
            liquidation_price,
            quantity,
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
            latest_quotes: LatestQuotes::default(),
            cfds: None,
            offers: MakerOffers::default(),
        }
    }

    async fn update_cfd(&mut self, db: sqlite_db::Connection, id: OrderId) -> Result<()> {
        let cfd = db.load_open_cfd(id, self.network).await?;

        let cfds = self
            .cfds
            .as_mut()
            .context("CFD list has not been initialized yet")?;

        cfds.insert(id, cfd);

        Ok(())
    }

    fn update_quotes(&mut self, quotes: LatestQuotes) {
        self.latest_quotes = quotes;
    }

    fn update_offers(&mut self, new_offers: Vec<CfdOffer>) {
        for new_offer in new_offers.into_iter() {
            match &new_offer {
                CfdOffer {
                    contract_symbol: ContractSymbol::BtcUsd,
                    position_maker: Position::Long,
                    ..
                } => self.offers.btcusd_long = Some(new_offer),
                CfdOffer {
                    contract_symbol: ContractSymbol::BtcUsd,
                    position_maker: Position::Short,
                    ..
                } => self.offers.btcusd_short = Some(new_offer),
                CfdOffer {
                    contract_symbol: ContractSymbol::EthUsd,
                    position_maker: Position::Long,
                    ..
                } => self.offers.ethusd_long = Some(new_offer),
                CfdOffer {
                    contract_symbol: ContractSymbol::EthUsd,
                    position_maker: Position::Short,
                    ..
                } => self.offers.ethusd_short = Some(new_offer),
            }
        }
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: Initialize) {
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
            &self.state.latest_quotes,
        );
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
            &self.state.latest_quotes,
        );
    }

    fn handle(&mut self, msg: Update<Vec<model::Offer>>) {
        let new_offers = msg
            .0
            .into_iter()
            .filter_map(|offer| match CfdOffer::new(offer, self.role) {
                Ok(offer) => Some(offer),
                Err(e) => {
                    tracing::warn!("Failed to build CfdOffer from model::Offer: {e:#}");
                    None
                }
            })
            .collect_vec();

        self.state.update_offers(new_offers);

        if let Err(e) = self.tx.send_offer_update(self.state.offers.clone()) {
            tracing::error!("Failed to propagate offer update: {e:#}");
        }
    }

    fn handle(&mut self, msg: Update<LatestQuotes>) {
        self.state.update_quotes(msg.0.clone());
        self.tx.send_quotes_update(msg.0.clone());

        let hydrated_cfds = match self.state.cfds.clone() {
            None => {
                tracing::debug!("Cannot update CFDs with new quote until they are initialized.");
                return;
            }
            Some(cfds) => cfds,
        };

        self.tx.send_cfds_update(hydrated_cfds, &msg.0);
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we just started");
        this.send_async_next(Initialize).await;

        tokio_extras::spawn(&this.clone(), {
            let price_feed = self.price_feed.clone();

            async move {
                loop {
                    {
                        let span = info_span!("Update projection with latest quote");
                        let latest = price_feed
                            .send(GetLatestQuotes)
                            .instrument(span.clone())
                            .await;

                        match latest {
                            Ok(quotes) => {
                                let _ = this
                                    .send(Update(into_projection_quotes(quotes)))
                                    .instrument(span)
                                    .await;
                            }
                            Err(_) => {
                                span.in_scope(|| {
                                    tracing::trace!("Price feed actor currently unreachable")
                                });
                            }
                        }
                    }

                    tokio_extras::time::sleep_silent(Duration::from_secs(10)).await;
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

pub type LatestQuotes = HashMap<ContractSymbol, Quote>;

/// Converts between ContractSymbol types
fn as_contract_symbol(symbol: &xtra_bitmex_price_feed::ContractSymbol) -> ContractSymbol {
    match symbol {
        xtra_bitmex_price_feed::ContractSymbol::BtcUsd => ContractSymbol::BtcUsd,
        xtra_bitmex_price_feed::ContractSymbol::EthUsd => ContractSymbol::EthUsd,
    }
}

/// Converts quotes from xtra_bitmex_price_feed into projection types
fn into_projection_quotes(latest_quotes: xtra_bitmex_price_feed::LatestQuotes) -> LatestQuotes {
    latest_quotes
        .iter()
        .map(|(symbol, quote)| (as_contract_symbol(symbol), (*quote).into()))
        .collect()
}

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct MakerOffers {
    pub btcusd_long: Option<CfdOffer>,
    pub btcusd_short: Option<CfdOffer>,
    pub ethusd_long: Option<CfdOffer>,
    pub ethusd_short: Option<CfdOffer>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CfdOffer {
    pub id: OfferId,

    pub contract_symbol: ContractSymbol,

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
    /// This is an estimate as the funding rate can fluctuate
    pub funding_rate_annualized_percent: String,

    /// The current estimated funding rate by the hour
    ///
    /// This represents the current funding rate of the maker.
    /// The funding rate fluctuates with market movements.
    pub funding_rate_hourly_percent: String,

    #[serde(with = "round_to_two_dp")]
    pub min_quantity: Contracts,
    #[serde(with = "round_to_two_dp")]
    pub max_quantity: Contracts,

    /// The user can only buy contracts in multiples of this.
    ///
    /// For example, if `lot_size` is 100, `min_quantity` is 300 and `max_quantity`is 800, then
    /// the user can buy 300, 400, 500, 600, 700 or 800 contracts.
    pub lot_size: LotSize,

    /// Contains liquidation price, margin and initial fund amount per leverage
    pub leverage_details: Vec<LeverageDetails>,

    pub creation_timestamp: Timestamp,
    pub settlement_time_interval_in_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LeverageDetails {
    pub leverage: Leverage,
    /// Own liquidation price according to position and leverage
    #[serde(with = "round_to_two_dp")]
    pub liquidation_price: Price,
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

impl CfdOffer {
    fn new(offer: model::Offer, role: Role) -> Result<Self> {
        let lot_size = offer.lot_size;

        let own_position = match role {
            Role::Maker => offer.position_maker,
            Role::Taker => offer.position_maker.counter_position(),
        };

        let leverage_details = offer
            .leverage_choices
            .iter()
            .map(|leverage| {
                let liquidation_price = match own_position {
                    Position::Long => calculate_long_liquidation_price(
                        offer.price,
                        offer.max_quantity,
                        *leverage,
                        offer.contract_symbol,
                    ),
                    Position::Short => calculate_short_liquidation_price(
                        offer.price,
                        offer.max_quantity,
                        *leverage,
                        offer.contract_symbol,
                    ),
                };
                // Margin per lot price is dependent on one's own leverage
                let margin_per_lot = calculate_margin(
                    offer.contract_symbol,
                    offer.price,
                    lot_size.into(),
                    *leverage,
                );

                let (long_leverage, short_leverage) =
                    long_and_short_leverage(*leverage, role, own_position);

                let initial_funding_fee_per_lot = FundingFee::calculate(
                    offer.price,
                    lot_size.into(),
                    long_leverage,
                    short_leverage,
                    offer.funding_rate,
                    SETTLEMENT_INTERVAL.whole_hours(),
                    offer.contract_symbol,
                )
                .context("unable to calculate initial funding fee")?;

                // Use a temporary fee account to define the funding fee's sign
                let temp_fee_account = FeeAccount::new(own_position, role);
                let initial_funding_fee_per_lot = temp_fee_account
                    .add_funding_fee(initial_funding_fee_per_lot)
                    .balance();

                Ok(LeverageDetails {
                    leverage: *leverage,
                    liquidation_price,
                    margin_per_lot,
                    initial_funding_fee_per_lot,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            id: offer.id,
            contract_symbol: offer.contract_symbol,
            position_maker: offer.position_maker,
            price: offer.price,
            min_quantity: offer.min_quantity,
            max_quantity: offer.max_quantity,
            lot_size,
            leverage_details,
            creation_timestamp: offer.creation_timestamp_maker,
            settlement_time_interval_in_secs: offer
                .settlement_interval
                .whole_seconds()
                .try_into()
                .context("unable to convert settlement interval")?,
            opening_fee: Some(offer.opening_fee.to_inner()),
            funding_rate_annualized_percent: AnnualisedFundingPercent::from(offer.funding_rate)
                .to_string(),
            funding_rate_hourly_percent: HourlyFundingPercent::from(offer.funding_rate).to_string(),
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
}

mod round_to_two_dp {
    use super::*;
    use serde::Serializer;

    pub trait ToDecimal {
        fn to_decimal(&self) -> Decimal;
    }

    impl ToDecimal for Contracts {
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
            let quantity = WithOnlyTwoDecimalPlaces {
                inner: model::Contracts::new(1000),
            };

            assert_ser_tokens(&quantity, &[Token::Str("1000")]);
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
        let _ = write!(self.url, ":{index}");
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

struct AnnualisedFundingPercent(Decimal);

impl From<FundingRate> for AnnualisedFundingPercent {
    fn from(funding_rate: FundingRate) -> Self {
        Self(
            funding_rate
                .to_decimal()
                .checked_mul(dec!(100))
                .expect("Not to overflow for funding rate")
                .checked_mul(Decimal::from(
                    (24 / SETTLEMENT_INTERVAL.whole_hours()) * 365,
                ))
                .expect("not to overflow"),
        )
    }
}

impl fmt::Display for AnnualisedFundingPercent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.round_dp(2).fmt(f)
    }
}

struct HourlyFundingPercent(Decimal);

impl From<FundingRate> for HourlyFundingPercent {
    fn from(funding_rate: FundingRate) -> Self {
        Self(
            funding_rate
                .to_decimal()
                .checked_mul(dec!(100))
                .expect("Not to overflow for funding rate")
                .checked_div(Decimal::from(SETTLEMENT_INTERVAL.whole_hours()))
                .expect("Not to fail as funding rate is sanitised"),
        )
    }
}

impl fmt::Display for HourlyFundingPercent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::OfferId;
    use model::OpeningFee;
    use model::TxFeeRate;
    use sqlite_db::memory;

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

    pub fn dummy_cfd() -> model::Cfd {
        model::Cfd::new(
            OrderId::default(),
            OfferId::default(),
            Position::Long,
            Price::new(dec!(60_000)).unwrap(),
            Leverage::TWO,
            time::Duration::hours(24),
            Role::Taker,
            Contracts::new(1_000),
            "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"
                .parse()
                .unwrap(),
            None,
            OpeningFee::new(Amount::from_sat(2000)),
            FundingRate::default(),
            TxFeeRate::default(),
            ContractSymbol::BtcUsd,
        )
    }

    pub fn setup_failed(cfd: &model::Cfd) -> CfdEvent {
        CfdEvent {
            timestamp: Timestamp::now(),
            id: cfd.id(),
            event: EventKind::ContractSetupFailed,
        }
    }

    pub fn order_rejected(cfd: &model::Cfd) -> CfdEvent {
        CfdEvent {
            timestamp: Timestamp::now(),
            id: cfd.id(),
            event: EventKind::OfferRejected,
        }
    }

    fn cfd_collaboratively_settled() -> (model::Cfd, CfdEvent, CfdEvent) {
        // 1|<RANDOM-ORDER-ID>|Long|41772.8325|2|24|100|
        // 69a42aa90da8b065b9532b62bff940a3ba07dbbb11d4482c7db83a7e049a9f1e|Taker|0|0|1
        let order_id = OrderId::default();
        let offer_id = OfferId::default();
        let cfd = model::Cfd::new(
            order_id,
            offer_id,
            Position::Long,
            Price::new(dec!(41_772.8325)).unwrap(),
            Leverage::TWO,
            time::Duration::hours(24),
            Role::Taker,
            Contracts::new(100),
            "69a42aa90da8b065b9532b62bff940a3ba07dbbb11d4482c7db83a7e049a9f1e"
                .parse()
                .unwrap(),
            None,
            OpeningFee::new(Amount::ZERO),
            FundingRate::default(),
            TxFeeRate::default(),
            ContractSymbol::BtcUsd,
        );

        let contract_setup_completed =
            std::fs::read_to_string("../sqlite-db/src/test_events/contract_setup_completed.json")
                .unwrap();
        let contract_setup_completed =
            serde_json::from_str::<EventKind>(&contract_setup_completed).unwrap();
        let contract_setup_completed = CfdEvent {
            timestamp: Timestamp::now(),
            id: order_id,
            event: contract_setup_completed,
        };

        let collaborative_settlement_completed = std::fs::read_to_string(
            "../sqlite-db/src/test_events/collaborative_settlement_completed.json",
        )
        .unwrap();
        let collaborative_settlement_completed =
            serde_json::from_str::<EventKind>(&collaborative_settlement_completed).unwrap();
        let collaborative_settlement_completed = CfdEvent {
            timestamp: Timestamp::now(),
            id: order_id,
            event: collaborative_settlement_completed,
        };

        (
            cfd,
            contract_setup_completed,
            collaborative_settlement_completed,
        )
    }

    fn collab_settlement_confirmed(cfd: &model::Cfd) -> CfdEvent {
        CfdEvent {
            timestamp: Timestamp::now(),
            id: cfd.id(),
            event: EventKind::CollaborativeSettlementConfirmed,
        }
    }

    #[tokio::test]
    async fn given_contract_setup_failed_when_move_cfds_to_failed_table_then_projection_aggregate_stays_the_same(
    ) {
        let db = memory().await.unwrap();

        let cfd = dummy_cfd();
        let order_id = cfd.id();

        db.insert_cfd(&cfd).await.unwrap();

        db.append_event(setup_failed(&cfd)).await.unwrap();

        let projection_open = {
            let projection_open = db
                .load_open_cfd::<Cfd>(order_id, bdk::bitcoin::Network::Testnet)
                .await
                .unwrap();
            projection_open.with_current_quote(None) // unconditional processing in `projection`
        };

        db.move_to_failed_cfds().await.unwrap();

        let projection_failed = {
            let projection_failed = db
                .load_failed_cfd::<Cfd>(order_id, bdk::bitcoin::Network::Testnet)
                .await
                .unwrap();
            projection_failed.with_current_quote(None) // unconditional processing in `projection`
        };

        // this comparison actually omits the `aggregated` field on
        // `projection::Cfd` because it is not used when aggregating
        // from a failed CFD
        assert_eq!(projection_open, projection_failed);
    }

    #[tokio::test]
    async fn given_order_rejected_when_move_cfds_to_failed_table_then_projection_aggregate_stays_the_same(
    ) {
        let db = memory().await.unwrap();

        let cfd = dummy_cfd();
        let order_id = cfd.id();

        db.insert_cfd(&cfd).await.unwrap();

        db.append_event(order_rejected(&cfd)).await.unwrap();

        let projection_open = {
            let projection_open = db
                .load_open_cfd::<Cfd>(order_id, bdk::bitcoin::Network::Testnet)
                .await
                .unwrap();
            projection_open.with_current_quote(None) // unconditional processing in `projection`
        };

        db.move_to_failed_cfds().await.unwrap();

        let projection_failed = {
            let projection_failed = db
                .load_failed_cfd::<Cfd>(order_id, bdk::bitcoin::Network::Testnet)
                .await
                .unwrap();
            projection_failed.with_current_quote(None) // unconditional processing in `projection`
        };

        // this comparison actually omits the `aggregated` field on
        // `projection::Cfd` because it is not used when aggregating
        // from a failed CFD
        assert_eq!(projection_open, projection_failed);
    }

    #[tokio::test]
    async fn given_confirmed_settlement_when_move_cfds_to_closed_table_then_projection_aggregate_stays_the_same(
    ) {
        let db = memory().await.unwrap();

        let (cfd, contract_setup_completed, collaborative_settlement_completed) =
            cfd_collaboratively_settled();
        let order_id = cfd.id();

        db.insert_cfd(&cfd).await.unwrap();

        db.append_event(contract_setup_completed).await.unwrap();
        db.append_event(collaborative_settlement_completed)
            .await
            .unwrap();

        db.append_event(collab_settlement_confirmed(&cfd))
            .await
            .unwrap();

        let projection_open = {
            let projection_open = db
                .load_open_cfd::<Cfd>(order_id, bdk::bitcoin::Network::Testnet)
                .await
                .unwrap();
            projection_open.with_current_quote(None) // unconditional processing in `projection`
        };

        db.move_to_closed_cfds().await.unwrap();

        let projection_closed = {
            let projection_closed = db
                .load_closed_cfd::<Cfd>(order_id, bdk::bitcoin::Network::Testnet)
                .await
                .unwrap();
            projection_closed.with_current_quote(None) // unconditional processing in `projection`
        };

        // this comparison actually omits the `aggregated` field on
        // `projection::Cfd` because it is not used when aggregating
        // from a closed CFD
        assert_eq!(projection_open, projection_closed);
    }
}
