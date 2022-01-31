use crate::bitmex_price_feed;
use crate::db;
use crate::model;
use crate::model::calculate_funding_fee;
use crate::model::cfd::calculate_long_liquidation_price;
use crate::model::cfd::calculate_long_margin;
use crate::model::cfd::calculate_profit;
use crate::model::cfd::calculate_profit_at_price;
use crate::model::cfd::calculate_short_margin;
use crate::model::cfd::CfdEvent;
use crate::model::cfd::Dlc;
use crate::model::cfd::Event;
use crate::model::cfd::OrderId;
use crate::model::cfd::Origin;
use crate::model::cfd::Role;
use crate::model::FeeAccount;
use crate::model::FundingRate;
use crate::model::Identity;
use crate::model::Leverage;
use crate::model::Position;
use crate::model::Price;
use crate::model::Timestamp;
use crate::model::TradingPair;
use crate::model::Usd;
use crate::xtra_ext::SendAsyncSafe;
use crate::Order;
use crate::Tasks;
use crate::SETTLEMENT_INTERVAL;
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
use maia::TransactionExt;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use serde::Serialize;
use sqlx::pool::PoolConnection;
use std::collections::HashSet;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::sync::watch;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;

/// Store the latest state of `T` for display purposes
/// (replaces previously stored values)
#[derive(Debug)]
pub struct Update<T>(pub T);

/// Message indicating that the Cfds in the projection need to be reloaded, as at
/// least one of the Cfds has changed.
#[derive(Debug)]
pub struct CfdsChanged;

pub struct Actor {
    db: sqlx::SqlitePool,
    tx: Tx,
    state: State,
    price_feed: Box<dyn MessageChannel<bitmex_price_feed::LatestQuote>>,
    tasks: Tasks,
}

pub struct Feeds {
    pub quote: watch::Receiver<Option<Quote>>,
    pub order: watch::Receiver<Option<CfdOrder>>,
    pub connected_takers: watch::Receiver<Vec<Identity>>,
    pub cfds: watch::Receiver<Vec<Cfd>>,
}

impl Actor {
    pub fn new(
        db: sqlx::SqlitePool,
        _role: Role,
        network: Network,
        price_feed: &(impl MessageChannel<bitmex_price_feed::LatestQuote> + 'static),
    ) -> (Self, Feeds) {
        let (tx_cfds, rx_cfds) = watch::channel(Vec::new());
        let (tx_order, rx_order) = watch::channel(None);
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
            order: rx_order,
            quote: rx_quote,
            connected_takers: rx_connected_takers,
        };

        (actor, feeds)
    }

    async fn refresh_cfds(&mut self) {
        let mut conn = match self.db.acquire().await {
            Ok(conn) => conn,
            Err(e) => {
                tracing::warn!("Failed to acquire DB connection: {e}");
                return;
            }
        };
        let cfds =
            match load_and_hydrate_cfds(&mut conn, self.state.quote, self.state.network).await {
                Ok(cfds) => cfds,
                Err(e) => {
                    tracing::warn!("Failed to load CFDs: {e:#}");
                    return;
                }
            };

        let _ = self.tx.cfds.send(cfds);
    }
}

async fn load_and_hydrate_cfds(
    conn: &mut PoolConnection<sqlx::Sqlite>,
    quote: Option<bitmex_price_feed::Quote>,
    network: Network,
) -> Result<Vec<Cfd>> {
    let ids = db::load_all_cfd_ids(conn).await?;

    let mut cfds = Vec::with_capacity(ids.len());

    for id in ids {
        let (cfd, events) = db::load_cfd(id, conn).await?;
        let role = cfd.role;

        let cfd = events.into_iter().fold(Cfd::new(cfd, quote), |cfd, event| {
            cfd.apply(event, network, role)
        });

        cfds.push(cfd);
    }

    Ok(cfds)
}

#[derive(Clone, Debug, Serialize)]
pub struct Cfd {
    pub order_id: OrderId,
    #[serde(with = "round_to_two_dp")]
    pub initial_price: Price,

    /// Sum of all costs
    ///
    /// Includes the opening fee and all fees that were already charged.
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub accumulated_fees: SignedAmount,

    pub leverage: Leverage,
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

    pub state: CfdState,
    pub actions: HashSet<CfdAction>,

    // TODO: This `CfdDetails` wrapper is useless and could be removed, but that would be a
    // breaking API change
    pub details: CfdDetails,

    #[serde(with = "::time::serde::timestamp::option")]
    pub expiry_timestamp: Option<OffsetDateTime>,

    pub counterparty: Identity,

    #[serde(with = "round_to_two_dp::opt")]
    pub pending_settlement_proposal_price: Option<Price>,

    #[serde(skip)]
    aggregated: Aggregated,
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

    /// If this is present the cet has not been published
    timelocked_cet: Option<Transaction>,

    commit_published: bool,
    refund_published: bool,
}

impl Aggregated {
    fn new(fee_account: FeeAccount) -> Self {
        Self {
            fee_account,

            latest_dlc: None,
            collab_settlement_tx: None,
            cet: None,
            timelocked_cet: None,
            commit_published: false,
            refund_published: false,
        }
    }

    fn payout(self, role: Role) -> Option<Amount> {
        if let Some((tx, script)) = self.collab_settlement_tx {
            return Some(extract_payout_amount(tx, script));
        }

        let tx = self.cet.or(self.timelocked_cet)?;
        let dlc = self
            .latest_dlc
            .as_ref()
            .expect("dlc to be present when we have a cet");
        let script = dlc.script_pubkey_for(role);

        Some(extract_payout_amount(tx, script))
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

impl Cfd {
    fn new(
        db::Cfd {
            id,
            position,
            initial_price,
            leverage,
            quantity_usd,
            counterparty_network_identity,
            role,
            opening_fee,
            initial_funding_rate,
            ..
        }: db::Cfd,
        latest_quote: Option<bitmex_price_feed::Quote>,
    ) -> Self {
        let long_margin = calculate_long_margin(initial_price, quantity_usd, leverage);
        let short_margin = calculate_short_margin(initial_price, quantity_usd);

        let (margin, margin_counterparty) = match position {
            Position::Long => (long_margin, short_margin),
            Position::Short => (short_margin, long_margin),
        };
        let liquidation_price = calculate_long_liquidation_price(leverage, initial_price);

        let latest_price = match (latest_quote, role) {
            (None, _) => None,
            (Some(quote), Role::Maker) => Some(quote.for_maker()),
            (Some(quote), Role::Taker) => Some(quote.for_taker()),
        };

        let initial_funding_fee = calculate_funding_fee(
            initial_price,
            quantity_usd,
            leverage,
            initial_funding_rate,
            SETTLEMENT_INTERVAL.whole_hours(),
        )
        .expect("values from db to be sane");

        let fee_account = FeeAccount::new(position, role)
            .add_opening_fee(opening_fee)
            .add_funding_fee(initial_funding_fee);

        let (profit_btc_latest_price, profit_percent_latest_price, payout) = latest_price.and_then(|latest_price| {
            match calculate_profit_at_price(initial_price, latest_price, quantity_usd, leverage, fee_account) {
                Ok(profit) => Some(profit),
                Err(e) => {
                    tracing::warn!("Failed to calculate profit/loss {:#}", e);

                    None
                }
            }
        }).map(|(in_btc, in_percent, payout)| (Some(in_btc), Some(in_percent.round_dp(1).to_string()), Some(payout)))
            .unwrap_or_else(|| {
                tracing::debug!(order_id = %id, "Unable to calculate profit/loss without current price");

                (None, None, None)
            });

        let initial_actions = if role == Role::Maker {
            HashSet::from([CfdAction::AcceptOrder, CfdAction::RejectOrder])
        } else {
            HashSet::new()
        };

        Self {
            order_id: id,
            initial_price,
            accumulated_fees: fee_account.balance(),
            leverage,
            trading_pair: TradingPair::BtcUsd,
            position,
            liquidation_price,
            quantity_usd,
            margin,
            margin_counterparty,
            role,

            // By default, we assume profit should be based on the latest price!
            profit_btc: profit_btc_latest_price,
            profit_percent: profit_percent_latest_price,
            payout,

            state: CfdState::PendingSetup,
            actions: initial_actions,
            details: CfdDetails {
                tx_url_list: HashSet::new(),
            },
            expiry_timestamp: None,
            counterparty: counterparty_network_identity,
            pending_settlement_proposal_price: None,
            aggregated: Aggregated::new(fee_account),
        }
    }

    fn apply(mut self, event: Event, network: Network, role: Role) -> Self {
        // First, try to set state based on event.
        use CfdEvent::*;
        match event.event {
            ContractSetupStarted => {
                self.state = CfdState::ContractSetup;
            }
            ContractSetupCompleted { dlc } => {
                self.aggregated.latest_dlc = Some(dlc);

                self.state = CfdState::PendingOpen;
            }
            ContractSetupFailed => {
                self.state = CfdState::SetupFailed;
            }
            OfferRejected => {
                self.state = CfdState::Rejected;
            }
            RolloverCompleted { dlc, funding_fee } => {
                self.aggregated.latest_dlc = Some(dlc);
                self.aggregated.fee_account =
                    self.aggregated.fee_account.add_funding_fee(funding_fee);
                self.accumulated_fees = self.aggregated.fee_account.balance();

                self.state = CfdState::Open;
            }
            RolloverRejected => {
                self.state = CfdState::Open;
            }
            RolloverFailed => {
                self.state = CfdState::Open;
            }
            CollaborativeSettlementStarted { proposal } => match role {
                Role::Maker => {
                    self.pending_settlement_proposal_price = Some(proposal.price);

                    self.state = CfdState::IncomingSettlementProposal;
                }
                Role::Taker => {
                    self.state = CfdState::OutgoingSettlementProposal;
                }
            },
            CollaborativeSettlementProposalAccepted => {
                self.pending_settlement_proposal_price = None;

                self.state = CfdState::PendingClose;
            }
            CollaborativeSettlementCompleted {
                spend_tx, script, ..
            } => {
                self.aggregated.collab_settlement_tx = Some((spend_tx, script));
                self.state = CfdState::PendingClose;
            }
            CollaborativeSettlementRejected { .. } => {
                self.aggregated.commit_published = true;
                self.pending_settlement_proposal_price = None;

                self.state = CfdState::PendingCommit;
            }
            CollaborativeSettlementFailed { .. } => {
                self.aggregated.commit_published = true;

                self.state = CfdState::PendingCommit;
            }
            LockConfirmed => {
                self.state = CfdState::Open;
            }
            CommitConfirmed => {
                // Commit can be published by either party, meaning it being confirmed might be the
                // first time we hear about it!
                self.aggregated.commit_published = true;

                self.state = CfdState::OpenCommitted;
            }
            CetConfirmed => {
                self.state = CfdState::Closed;
            }
            RefundConfirmed => {
                self.state = CfdState::Refunded;
            }
            CollaborativeSettlementConfirmed => {
                self.state = CfdState::Closed;
            }
            CetTimelockExpiredPriorOracleAttestation => {
                self.state = CfdState::OpenCommitted;
            }
            CetTimelockExpiredPostOracleAttestation { cet } => {
                self.aggregated.cet = Some(cet);

                self.state = CfdState::PendingCet;
            }
            RefundTimelockExpired { .. } => {
                self.aggregated.refund_published = true;

                self.state = CfdState::PendingRefund;
            }
            OracleAttestedPriorCetTimelock { timelocked_cet, .. } => {
                self.aggregated.timelocked_cet = Some(timelocked_cet);

                self.state = CfdState::PendingCommit;
            }
            OracleAttestedPostCetTimelock { cet, .. } => {
                self.aggregated.cet = Some(cet);
                self.state = CfdState::PendingCet;
            }
            ManualCommit { .. } => {
                self.aggregated.commit_published = true;

                self.state = CfdState::PendingCommit;
            }
            RevokeConfirmed => todo!("Deal with revoked"),
            RolloverStarted { .. } => match role {
                Role::Maker => {
                    self.state = CfdState::IncomingRolloverProposal;
                }
                Role::Taker => {
                    self.state = CfdState::OutgoingRolloverProposal;
                }
            },
            RolloverAccepted => {
                self.state = CfdState::ContractSetup;
            }
        };

        self.actions = self.derive_actions(role);

        // If we don't have a dedicated closing price, keep the one that is set (which is
        // based on current price).
        if let Some(payout) = self.aggregated.clone().payout(role) {
            let payout = payout
                .to_signed()
                .expect("Amount to fit into signed amount");

            let (profit_btc, profit_percent) = calculate_profit(
                payout,
                self.margin
                    .to_signed()
                    .expect("Amount to fit into signed amount"),
            );

            self.payout = Some(payout);
            self.profit_btc = Some(profit_btc);
            self.profit_percent = Some(profit_percent.to_string());
        }

        if let Some(lock_tx_url) = self.lock_tx_url(network) {
            self.details.tx_url_list.insert(lock_tx_url);
        }
        if let Some(commit_tx_url) = self.commit_tx_url(network) {
            self.details.tx_url_list.insert(commit_tx_url);
        }
        if let Some(collab_settlement_tx_url) = self.collab_settlement_tx_url(network) {
            self.details.tx_url_list.insert(collab_settlement_tx_url);
        }
        if let Some(refund_tx_url) = self.refund_tx_url(network, role) {
            self.details.tx_url_list.insert(refund_tx_url);
        }
        if let Some(cet_url) = self.cet_url(network, role) {
            self.details.tx_url_list.insert(cet_url);
        }

        self
    }

    fn derive_actions(&self, role: Role) -> HashSet<CfdAction> {
        match (self.state, role) {
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

    fn refund_tx_url(&self, network: Network, role: Role) -> Option<TxUrl> {
        if !self.aggregated.refund_published {
            return None;
        }

        let dlc = self.aggregated.latest_dlc.as_ref()?;

        let url = TxUrl::from_transaction(
            &dlc.refund.0,
            &dlc.script_pubkey_for(role),
            network,
            TxLabel::Refund,
        );

        Some(url)
    }

    fn cet_url(&self, network: Network, role: Role) -> Option<TxUrl> {
        let tx = self.aggregated.cet.as_ref()?;
        let dlc = self.aggregated.latest_dlc.as_ref()?;

        let url = TxUrl::from_transaction(tx, &dlc.script_pubkey_for(role), network, TxLabel::Cet);

        Some(url)
    }
}

/// Internal struct to keep all the senders around in one place
struct Tx {
    pub cfds: watch::Sender<Vec<Cfd>>,
    pub order: watch::Sender<Option<CfdOrder>>,
    pub quote: watch::Sender<Option<Quote>>,
    // TODO: Use this channel to communicate maker status as well with generic
    // ID of connected counterparties
    pub connected_takers: watch::Sender<Vec<Identity>>,
}

/// Internal struct to keep state in one place
struct State {
    network: Network,
    quote: Option<bitmex_price_feed::Quote>,
}

impl State {
    fn new(network: Network) -> Self {
        Self {
            network,
            quote: None,
        }
    }

    fn update_quote(&mut self, quote: Option<bitmex_price_feed::Quote>) {
        self.quote = quote;
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, _: CfdsChanged) {
        self.refresh_cfds().await
    }

    fn handle(&mut self, msg: Update<Option<Order>>) {
        let order = match msg.0 {
            None => None,
            Some(order) => match TryInto::<CfdOrder>::try_into(order) {
                Ok(order) => Some(order),
                Err(e) => {
                    tracing::warn!("Unable to convert order: {e:#}");
                    None
                }
            },
        };

        let _ = self.tx.order.send(order);
    }

    fn handle(&mut self, msg: Update<Option<bitmex_price_feed::Quote>>) {
        self.state.update_quote(msg.0);
        let _ = self.tx.quote.send(msg.0.map(|q| q.into()));
        self.refresh_cfds().await;
    }

    fn handle(&mut self, msg: Update<Vec<model::Identity>>) {
        let _ = self.tx.connected_takers.send(msg.0);
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we just started");

        // this will make us load all cfds from the DB
        this.send_async_safe(CfdsChanged)
            .await
            .expect("we just started");

        self.tasks.add({
            let price_feed = self.price_feed.clone_channel();

            async move {
                loop {
                    match price_feed.send(bitmex_price_feed::LatestQuote).await {
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
            bid: quote.bid,
            ask: quote.ask,
            last_updated_at: quote.timestamp,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CfdOrder {
    pub id: OrderId,

    pub trading_pair: TradingPair,
    pub position: Position,

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
    pub min_quantity: Usd,
    #[serde(with = "round_to_two_dp")]
    pub max_quantity: Usd,

    /// The user can only buy contracts in multiples of this.
    ///
    /// For example, if `parcel_size` is 100, `min_quantity` is 300 and `max_quantity`is 800, then
    /// the user can buy 300, 400, 500, 600, 700 or 800 contracts.
    #[serde(with = "round_to_two_dp")]
    pub parcel_size: Usd,

    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub margin_per_parcel: Amount,

    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub initial_funding_fee_per_parcel: Amount,

    pub leverage: Leverage,
    #[serde(with = "round_to_two_dp")]
    pub liquidation_price: Price,

    pub creation_timestamp: Timestamp,
    pub settlement_time_interval_in_secs: u64,
}

impl TryFrom<Order> for CfdOrder {
    type Error = anyhow::Error;

    fn try_from(order: Order) -> std::result::Result<Self, Self::Error> {
        let parcel_size = Usd::new(dec!(100)); // TODO: Have the maker tell us this.

        Ok(Self {
            id: order.id,
            trading_pair: order.trading_pair,
            position: order.position,
            price: order.price,
            min_quantity: order.min_quantity,
            max_quantity: order.max_quantity,
            parcel_size,
            margin_per_parcel: match (order.origin, order.position) {
                (Origin::Theirs, Position::Short) | (Origin::Ours, Position::Long) => {
                    calculate_long_margin(order.price, parcel_size, order.leverage)
                }
                (Origin::Ours, Position::Short) | (Origin::Theirs, Position::Long) => {
                    calculate_short_margin(order.price, parcel_size)
                }
            },
            leverage: order.leverage,
            liquidation_price: order.liquidation_price,
            creation_timestamp: order.creation_timestamp,
            settlement_time_interval_in_secs: order
                .settlement_interval
                .whole_seconds()
                .try_into()
                .context("unable to convert settlement interval")?,
            opening_fee: Some(order.opening_fee.to_inner()),
            funding_rate_annualized_percent: AnnualisedFundingPercent::from(order.funding_rate)
                .to_string(),
            funding_rate_hourly_percent: HourlyFundingPercent::from(order.funding_rate).to_string(),
            initial_funding_fee_per_parcel: calculate_funding_fee(
                order.price,
                parcel_size,
                order.leverage,
                order.funding_rate,
                SETTLEMENT_INTERVAL.whole_hours(),
            )
            .context("unable to calcualte initial funding fee")?
            .to_inner(),
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
    Closed,
    PendingRefund,
    Refunded,
    SetupFailed,
}

#[derive(Debug, Clone, Serialize)]
pub struct CfdDetails {
    tx_url_list: HashSet<TxUrl>,
}

#[derive(Debug, derive_more::Display, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, PartialEq, Serialize, Eq, Hash)]
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
}
