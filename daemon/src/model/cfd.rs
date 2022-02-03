use crate::model::calculate_funding_fee;
use crate::model::BitMexPriceEventId;
use crate::model::FeeAccount;
use crate::model::FundingFee;
use crate::model::FundingRate;
use crate::model::Identity;
use crate::model::InversePrice;
use crate::model::Leverage;
use crate::model::OpeningFee;
use crate::model::Percent;
use crate::model::Position;
use crate::model::Price;
use crate::model::Timestamp;
use crate::model::TradingPair;
use crate::model::TxFeeRate;
use crate::model::Usd;
use crate::oracle;
use crate::payout_curve;
use crate::setup_contract::RolloverParams;
use crate::setup_contract::SetupParams;
use crate::SETTLEMENT_INTERVAL;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use bdk::bitcoin::secp256k1::SecretKey;
use bdk::bitcoin::secp256k1::Signature;
use bdk::bitcoin::Address;
use bdk::bitcoin::Amount;
use bdk::bitcoin::PublicKey;
use bdk::bitcoin::Script;
use bdk::bitcoin::SignedAmount;
use bdk::bitcoin::Transaction;
use bdk::bitcoin::TxIn;
use bdk::bitcoin::TxOut;
use bdk::bitcoin::Txid;
use bdk::descriptor::Descriptor;
use bdk::miniscript::DescriptorTrait;
use cached::proc_macro::cached;
use maia::finalize_spend_transaction;
use maia::secp256k1_zkp;
use maia::secp256k1_zkp::EcdsaAdaptorSignature;
use maia::secp256k1_zkp::SECP256K1;
use maia::spending_tx_sighash;
use maia::TransactionExt;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::de::Error as _;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::ops::RangeInclusive;
use std::str;
use time::Duration;
use time::OffsetDateTime;
use uuid::adapter::Hyphenated;
use uuid::Uuid;

pub const CET_TIMELOCK: u32 = 12;

const CONTRACT_SETUP_COMPLETED_EVENT: &str = "ContractSetupCompleted";
const ROLLOVER_COMPLETED_EVENT: &str = "RolloverCompleted";

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct OrderId(Hyphenated);

impl Serialize for OrderId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for OrderId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let uuid = String::deserialize(deserializer)?;
        let uuid = uuid.parse::<Uuid>().map_err(D::Error::custom)?;

        Ok(Self(uuid.to_hyphenated()))
    }
}

impl Default for OrderId {
    fn default() -> Self {
        Self(Uuid::new_v4().to_hyphenated())
    }
}

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Uuid> for OrderId {
    fn from(id: Uuid) -> Self {
        OrderId(id.to_hyphenated())
    }
}

// TODO: Could potentially remove this and use the Role in the Order instead
/// Origin of the order
#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, sqlx::Type)]
pub enum Origin {
    Ours,
    Theirs,
}

/// Role in the Cfd
#[derive(Debug, Copy, Clone, PartialEq, sqlx::Type, Serialize, Deserialize)]
pub enum Role {
    Maker,
    Taker,
}

impl From<Origin> for Role {
    fn from(origin: Origin) -> Self {
        match origin {
            Origin::Ours => Role::Maker,
            Origin::Theirs => Role::Taker,
        }
    }
}

/// A concrete order created by a maker for a taker
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Order {
    pub id: OrderId,

    pub trading_pair: TradingPair,
    pub position: Position,

    pub price: Price,

    // TODO: [post-MVP] Representation of the contract size; at the moment the contract size is
    //  always 1 USD
    pub min_quantity: Usd,
    pub max_quantity: Usd,

    pub leverage: Leverage,

    // TODO: Remove from order, can be calculated
    pub liquidation_price: Price,

    pub creation_timestamp: Timestamp,

    /// The duration that will be used for calculating the settlement timestamp
    pub settlement_interval: Duration,

    pub origin: Origin,

    /// The id of the event to be used for price attestation
    ///
    /// The maker includes this into the Order based on the Oracle announcement to be used.
    pub oracle_event_id: BitMexPriceEventId,

    pub tx_fee_rate: TxFeeRate,
    pub funding_rate: FundingRate,
    pub opening_fee: OpeningFee,
}

impl Order {
    #[allow(clippy::too_many_arguments)]
    pub fn new_short(
        price: Price,
        min_quantity: Usd,
        max_quantity: Usd,
        origin: Origin,
        oracle_event_id: BitMexPriceEventId,
        settlement_interval: Duration,
        tx_fee_rate: TxFeeRate,
        funding_rate: FundingRate,
        opening_fee: OpeningFee,
    ) -> Result<Self> {
        let leverage = Leverage::new(2)?;
        let liquidation_price = calculate_long_liquidation_price(leverage, price);

        Ok(Order {
            id: OrderId::default(),
            price,
            min_quantity,
            max_quantity,
            leverage,
            trading_pair: TradingPair::BtcUsd,
            liquidation_price,
            position: Position::Short,
            creation_timestamp: Timestamp::now(),
            settlement_interval,
            origin,
            oracle_event_id,
            tx_fee_rate,
            funding_rate,
            opening_fee,
        })
    }
}

/// Proposed collaborative settlement
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SettlementProposal {
    pub order_id: OrderId,
    pub timestamp: Timestamp,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub taker: Amount,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
    pub maker: Amount,
    pub price: Price,
}

/// Proposed rollover
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RolloverProposal {
    pub order_id: OrderId,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone)]
pub enum SettlementKind {
    Incoming,
    Outgoing,
}

/// Reasons why we cannot rollover a CFD.
#[derive(thiserror::Error, Debug, PartialEq)]
pub enum NoRolloverReason {
    #[error("Is too recent to auto-rollover")]
    TooRecent,
    #[error("CFD does not have a DLC")]
    NoDlc,
    #[error("Cannot roll over when CFD not locked yet")]
    NotLocked,
    #[error("Cannot roll over when CFD is committed")]
    Committed,
    #[error("Cannot roll over when CFD is attested by oracle")]
    Attested,
    #[error("Cannot roll over while CFD is in collaborative settlement")]
    InCollaborativeSettlement,
    #[error("Cannot roll over when CFD is final")]
    Final,
}

/// Errors that can happen when handling the expiry of the refund
/// timelock on the commit transaciton.
#[derive(thiserror::Error, Debug)]
pub enum RefundTimelockExpiryError {
    #[error("CFD is already final")]
    AlreadyFinal,
    #[error("CFD does not have a DLC")]
    NoDlc,
    #[error("Failed to sign refund transaction")]
    Signing(#[from] anyhow::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub timestamp: Timestamp,
    pub id: OrderId,
    pub event: CfdEvent,
}

impl Event {
    pub fn new(id: OrderId, event: CfdEvent) -> Self {
        Event {
            timestamp: Timestamp::now(),
            id,
            event,
        }
    }
}

/// CfdEvents used by the maker and taker, some events are only for one role
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
#[serde(tag = "name", content = "data")]
pub enum CfdEvent {
    ContractSetupStarted,
    ContractSetupCompleted {
        dlc: Dlc,
    },

    ContractSetupFailed,
    OfferRejected,

    RolloverStarted,
    RolloverAccepted,
    RolloverRejected,
    RolloverCompleted {
        dlc: Dlc,
        funding_fee: FundingFee,
    },
    RolloverFailed,

    CollaborativeSettlementStarted {
        proposal: SettlementProposal,
    },
    CollaborativeSettlementProposalAccepted,
    CollaborativeSettlementCompleted {
        #[serde(with = "hex_transaction")]
        spend_tx: Transaction,
        script: Script,
        price: Price,
    },
    CollaborativeSettlementRejected,
    // TODO: We can distinguish different "failed" scenarios and potentially decide to publish the
    // commit transaction for some
    CollaborativeSettlementFailed,

    // TODO: The monitoring events should move into the monitor once we use multiple
    // aggregates in different actors
    LockConfirmed,
    /// The lock transaction is confirmed after CFD is already final
    ///
    /// This can happen in cases where we publish a settlement transaction while the lock
    /// transaction is still pending and they end up in the same block.
    LockConfirmedAfterFinality,
    CommitConfirmed,
    CetConfirmed,
    RefundConfirmed,
    RevokeConfirmed,
    CollaborativeSettlementConfirmed,

    CetTimelockExpiredPriorOracleAttestation,
    CetTimelockExpiredPostOracleAttestation {
        #[serde(with = "hex_transaction")]
        cet: Transaction,
    },

    RefundTimelockExpired {
        #[serde(with = "hex_transaction")]
        refund_tx: Transaction,
    },

    // TODO: Once we use multiple aggregates in different actors we could change this to something
    // like CetReadyForPublication that is emitted by the CfdActor. The Oracle actor would
    // take care of saving and broadcasting an attestation event that can be picked up by the
    // wallet actor which can then decide to publish the CetReadyForPublication event.
    OracleAttestedPriorCetTimelock {
        #[serde(with = "hex_transaction")]
        timelocked_cet: Transaction,
        /// The commit transaction for the DLC of this CFD.
        ///
        /// If this is set to `Some`, we haven't previously attempted broadcast `commit_tx` and
        /// need to broadcast it as a result of this event.
        #[serde(with = "hex_transaction::opt")]
        commit_tx: Option<Transaction>,
        price: Price,
    },
    OracleAttestedPostCetTimelock {
        #[serde(with = "hex_transaction")]
        cet: Transaction,
        price: Price,
    },
    ManualCommit {
        #[serde(with = "hex_transaction")]
        tx: Transaction,
    },
}

impl CfdEvent {
    pub fn to_json(&self) -> (String, String) {
        let value = serde_json::to_value(self).expect("serialization to always work");
        let object = value.as_object().expect("always an object");

        let name = object
            .get("name")
            .expect("to have property `name`")
            .as_str()
            .expect("name to be `string`")
            .to_owned();
        let data = object.get("data").cloned().unwrap_or_default().to_string();

        (name, data)
    }

    pub fn from_json(name: String, data: String) -> Result<Self> {
        match name.as_str() {
            CONTRACT_SETUP_COMPLETED_EVENT | ROLLOVER_COMPLETED_EVENT => {
                from_json_inner_cached(name, data)
            }
            _ => from_json_inner(name, data),
        }
    }
}

// Deserialisation of events has been proved to use substantial amount of the CPU.
// Cache the events.
#[cached(size = 500, result = true)]
fn from_json_inner_cached(name: String, data: String) -> Result<CfdEvent> {
    from_json_inner(name, data)
}

fn from_json_inner(name: String, data: String) -> Result<CfdEvent> {
    use serde_json::json;

    let data = serde_json::from_str::<serde_json::Value>(&data)?;

    let event = serde_json::from_value::<CfdEvent>(json!({
        "name": name,
        "data": data
    }))?;

    Ok(event)
}

/// Models the cfd state of the taker
///
/// Upon `Command`s, that are reaction to something happening in the system, we decide to
/// produce `Event`s that are saved in the database. After saving an `Event` in the database
/// we apply the event to the aggregate producing a new aggregate (representing the latest state
/// `version`). To bring a cfd into a certain state version we load all events from the
/// database and apply them in order (order by version).
#[derive(Debug, PartialEq)]
pub struct Cfd {
    version: u64,

    // static
    id: OrderId,
    position: Position,
    initial_price: Price,
    initial_funding_rate: FundingRate,
    leverage: Leverage,
    settlement_interval: Duration,
    quantity: Usd,
    counterparty_network_identity: Identity,
    role: Role,
    opening_fee: OpeningFee,
    initial_tx_fee_rate: TxFeeRate,
    // dynamic (based on events)
    fee_account: FeeAccount,

    dlc: Option<Dlc>,

    /// Holds the decrypted CET transaction if we have previously emitted it as part of an event.
    ///
    /// There is not guarantee that the transaction is confirmed if this is set to `Some`.
    /// However, if this is set to `Some`, there is no need to re-emit it as part of another event.
    cet: Option<Transaction>,

    /// Holds the decrypted commit transaction if we have previously emitted it as part of an
    /// event.
    ///
    /// There is not guarantee that the transaction is confirmed if this is set to `Some`.
    /// However, if this is set to `Some`, there is no need to re-emit it as part of another event.
    commit_tx: Option<Transaction>,

    collaborative_settlement_spend_tx: Option<Transaction>,
    refund_tx: Option<Transaction>,

    lock_finality: bool,

    commit_finality: bool,
    refund_finality: bool,
    cet_finality: bool,
    collaborative_settlement_finality: bool,
    cet_timelock_expired: bool,

    refund_timelock_expired: bool,

    during_contract_setup: bool,
    during_rollover: bool,
    settlement_proposal: Option<SettlementProposal>,
}

impl Cfd {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: OrderId,
        position: Position,
        initial_price: Price,
        leverage: Leverage,
        settlement_interval: Duration, /* TODO: Make a newtype that enforces hours only so
                                        * we don't have to deal with precisions in the
                                        * database. */
        role: Role,
        quantity: Usd,
        counterparty_network_identity: Identity,
        opening_fee: OpeningFee,
        initial_funding_rate: FundingRate,
        initial_tx_fee_rate: TxFeeRate,
    ) -> Self {
        let initial_funding_fee = calculate_funding_fee(
            initial_price,
            quantity,
            leverage,
            initial_funding_rate,
            SETTLEMENT_INTERVAL.whole_hours(),
        )
        .expect("values from db to be sane");

        Cfd {
            version: 0,
            id,
            position,
            initial_price,
            leverage,
            settlement_interval,
            quantity,
            counterparty_network_identity,
            role,
            initial_funding_rate,
            opening_fee,
            initial_tx_fee_rate,
            dlc: None,
            cet: None,
            commit_tx: None,
            collaborative_settlement_spend_tx: None,
            refund_tx: None,
            lock_finality: false,
            commit_finality: false,
            refund_finality: false,
            cet_finality: false,
            collaborative_settlement_finality: false,
            cet_timelock_expired: false,
            refund_timelock_expired: false,
            during_contract_setup: false,
            during_rollover: false,
            settlement_proposal: None,
            fee_account: FeeAccount::new(position, role)
                .add_opening_fee(opening_fee)
                .add_funding_fee(initial_funding_fee),
        }
    }

    /// A convenience method, creating a Cfd from an Order
    pub fn from_order(
        order: Order,
        position: Position,
        quantity: Usd,
        counterparty_network_identity: Identity,
        role: Role,
    ) -> Self {
        Cfd::new(
            order.id,
            position,
            order.price,
            order.leverage,
            order.settlement_interval,
            role,
            quantity,
            counterparty_network_identity,
            order.opening_fee,
            order.funding_rate,
            order.tx_fee_rate,
        )
    }

    /// Creates a new [`Cfd`] and rehydrates it from the given list of events.
    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate(
        id: OrderId,
        position: Position,
        initial_price: Price,
        leverage: Leverage,
        settlement_interval: Duration,
        quantity: Usd,
        counterparty_network_identity: Identity,
        role: Role,
        opening_fee: OpeningFee,
        initial_funding_rate: FundingRate,
        initial_tx_fee_rate: TxFeeRate,
        events: Vec<Event>,
    ) -> Self {
        let cfd = Self::new(
            id,
            position,
            initial_price,
            leverage,
            settlement_interval,
            role,
            quantity,
            counterparty_network_identity,
            opening_fee,
            initial_funding_rate,
            initial_tx_fee_rate,
        );
        events.into_iter().fold(cfd, Cfd::apply)
    }

    fn expiry_timestamp(&self) -> Option<OffsetDateTime> {
        self.dlc
            .as_ref()
            .map(|dlc| dlc.settlement_event_id.timestamp)
    }

    fn margin(&self) -> Amount {
        match self.position {
            Position::Long => {
                calculate_long_margin(self.initial_price, self.quantity, self.leverage)
            }
            Position::Short => calculate_short_margin(self.initial_price, self.quantity),
        }
    }

    fn counterparty_margin(&self) -> Amount {
        match self.position {
            Position::Short => {
                calculate_long_margin(self.initial_price, self.quantity, self.leverage)
            }
            Position::Long => calculate_short_margin(self.initial_price, self.quantity),
        }
    }

    fn is_in_collaborative_settlement(&self) -> bool {
        self.settlement_proposal.is_some()
    }

    pub fn can_auto_rollover_taker(&self, now: OffsetDateTime) -> Result<(), NoRolloverReason> {
        let expiry_timestamp = self.expiry_timestamp().ok_or(NoRolloverReason::NoDlc)?;
        let time_until_expiry = expiry_timestamp - now;
        if time_until_expiry > SETTLEMENT_INTERVAL - Duration::HOUR {
            return Err(NoRolloverReason::TooRecent);
        }

        self.can_rollover()?;

        Ok(())
    }

    fn can_rollover(&self) -> Result<(), NoRolloverReason> {
        if self.is_final() {
            return Err(NoRolloverReason::Final);
        }

        if self.is_attested() {
            return Err(NoRolloverReason::Attested);
        }

        if self.commit_finality {
            return Err(NoRolloverReason::Committed);
        }

        if !self.lock_finality {
            return Err(NoRolloverReason::NotLocked);
        }

        // Rollover and collaborative settlement are mutually exclusive, if we are currently
        // collaboratively settling we cannot roll over
        if self.is_in_collaborative_settlement() || self.collaborative_settlement_spend_tx.is_some()
        {
            return Err(NoRolloverReason::InCollaborativeSettlement);
        }

        Ok(())
    }

    fn can_settle_collaboratively(&self) -> bool {
        !self.commit_finality && !self.is_final() && !self.is_attested()
            // Rollover and collaborative settlement are mutually exclusive, if we are currently rolling over we cannot settle
            && !self.during_rollover
    }

    fn is_attested(&self) -> bool {
        self.cet.is_some()
    }

    fn is_final(&self) -> bool {
        self.collaborative_settlement_finality || self.cet_finality || self.refund_finality
    }

    pub fn start_contract_setup(&self) -> Result<(Event, SetupParams)> {
        if self.version > 0 {
            bail!("Start contract not allowed in version {}", self.version)
        }

        let margin = self.margin();
        let counterparty_margin = self.counterparty_margin();

        Ok((
            Event::new(self.id(), CfdEvent::ContractSetupStarted),
            SetupParams::new(
                margin,
                counterparty_margin,
                self.counterparty_network_identity,
                self.initial_price,
                self.quantity,
                self.leverage,
                self.refund_timelock_in_blocks(),
                self.initial_tx_fee_rate(),
                self.fee_account,
            )?,
        ))
    }

    pub fn start_rollover(&self) -> Result<Event> {
        if self.during_rollover {
            bail!("The CFD is already being rolled over")
        };

        self.can_rollover()?;

        Ok(Event::new(self.id, CfdEvent::RolloverStarted))
    }

    pub fn accept_rollover_proposal(
        self,
        tx_fee_rate: TxFeeRate,
        funding_rate: FundingRate,
    ) -> Result<(Event, RolloverParams, Dlc, Duration)> {
        if !self.during_rollover {
            bail!("The CFD is not rolling over");
        }

        if self.role != Role::Maker {
            bail!("Can only accept proposal as a maker");
        }

        let hours_to_charge = 1;

        let funding_fee = calculate_funding_fee(
            self.initial_price,
            self.quantity,
            self.leverage,
            funding_rate,
            hours_to_charge,
        )?;

        Ok((
            Event::new(self.id, CfdEvent::RolloverAccepted),
            RolloverParams::new(
                self.initial_price,
                self.quantity,
                self.leverage,
                self.refund_timelock_in_blocks(),
                tx_fee_rate,
                self.fee_account,
                funding_fee,
            ),
            self.dlc.clone().context("No DLC present")?,
            self.settlement_interval,
        ))
    }

    pub fn handle_rollover_accepted_taker(
        &self,
        tx_fee_rate: TxFeeRate,
        funding_rate: FundingRate,
    ) -> Result<(Event, RolloverParams, Dlc)> {
        if !self.during_rollover {
            bail!("The CFD is not rolling over");
        }

        if self.role != Role::Taker {
            bail!("Can only handle accepted proposal as a taker");
        }

        self.can_rollover()?;

        // TODO: Taker should take this from the maker, optionally calculate and verify
        // whether they match
        let hours_to_charge = 1;
        let funding_fee = calculate_funding_fee(
            self.initial_price,
            self.quantity,
            self.leverage,
            funding_rate,
            hours_to_charge,
        )?;

        Ok((
            self.event(CfdEvent::RolloverAccepted),
            RolloverParams::new(
                self.initial_price,
                self.quantity,
                self.leverage,
                self.refund_timelock_in_blocks(),
                tx_fee_rate,
                self.fee_account,
                funding_fee,
            ),
            self.dlc.clone().context("No DLC present")?,
        ))
    }

    pub fn sign_collaborative_settlement_maker(
        &self,
        proposal: SettlementProposal,
        sig_taker: Signature,
    ) -> Result<CollaborativeSettlement> {
        let dlc = self
            .dlc
            .as_ref()
            .context("dlc has to be available for collab settlemment")?
            .clone();

        let (tx, sig_maker) = dlc.close_transaction(&proposal)?;
        let spend_tx = dlc.finalize_spend_transaction((tx, sig_maker), sig_taker)?;
        let script_pk = dlc.script_pubkey_for(Role::Maker);

        let settlement = CollaborativeSettlement::new(spend_tx, script_pk, proposal.price)?;
        Ok(settlement)
    }

    pub fn propose_collaborative_settlement(
        &self,
        current_price: Price,
        n_payouts: usize,
    ) -> Result<Event> {
        anyhow::ensure!(
            !self.is_in_collaborative_settlement()
                && self.role == Role::Taker
                && self.can_settle_collaboratively(),
            "Failed to propose collaborative settlement"
        );

        let payout_curve = payout_curve::calculate(
            self.initial_price,
            self.quantity,
            self.leverage,
            n_payouts,
            self.fee_account.settle(),
        )?;

        let payout = {
            let current_price = current_price.try_into_u64()?;
            payout_curve
                .iter()
                .find(|&x| x.digits().range().contains(&current_price))
                .context("find current price on the payout curve")?
        };

        let proposal = SettlementProposal {
            order_id: self.id,
            timestamp: Timestamp::now(),
            taker: *payout.taker_amount(),
            maker: *payout.maker_amount(),
            price: current_price,
        };

        Ok(Event::new(
            self.id,
            CfdEvent::CollaborativeSettlementStarted { proposal },
        ))
    }

    pub fn receive_collaborative_settlement_proposal(
        self,
        proposal: SettlementProposal,
        n_payouts: usize,
    ) -> Result<Event> {
        anyhow::ensure!(
            !self.is_in_collaborative_settlement()
                && self.role == Role::Maker
                && self.can_settle_collaboratively()
                && proposal.order_id == self.id,
            "Failed to start collaborative settlement"
        );

        // Validate that the amounts sent by the taker are sane according to the payout curve

        let payout_curve_long = payout_curve::calculate(
            self.initial_price,
            self.quantity,
            self.leverage,
            n_payouts,
            self.fee_account.settle(),
        )?;

        let payout = {
            let proposal_price = proposal.price.try_into_u64()?;
            payout_curve_long
                .iter()
                .find(|&x| x.digits().range().contains(&proposal_price))
                .context("find current price on the payout curve")?
        };

        if proposal.maker != *payout.maker_amount() || proposal.taker != *payout.taker_amount() {
            bail!("The settlement amounts sent by the taker are not according to the agreed payout curve. Expected taker {} and maker {} but received taker {} and maker {}", payout.taker_amount(), payout.maker_amount(), proposal.taker, proposal.maker);
        }

        Ok(Event::new(
            self.id,
            CfdEvent::CollaborativeSettlementStarted { proposal },
        ))
    }

    pub fn accept_collaborative_settlement_proposal(
        self,
        proposal: &SettlementProposal,
    ) -> Result<Event> {
        anyhow::ensure!(
            self.role == Role::Maker && self.settlement_proposal.as_ref() == Some(proposal)
        );

        Ok(Event::new(
            self.id,
            CfdEvent::CollaborativeSettlementProposalAccepted,
        ))
    }

    pub fn setup_contract(self, completed: SetupCompleted) -> Result<Event> {
        // Version 1 is acceptable, as it means that we started contract setup
        // TODO: Use self.during_contract_setup after introducing
        // ContractSetupAccepted event
        if self.version > 1 {
            bail!(
                "Complete contract setup not allowed because cfd in version {}",
                self.version
            )
        }

        let event = match completed {
            SetupCompleted::Succeeded {
                payload: (dlc, _), ..
            } => CfdEvent::ContractSetupCompleted { dlc },
            SetupCompleted::Rejected { .. } => CfdEvent::OfferRejected,
            SetupCompleted::Failed { error, .. } => {
                tracing::error!("Contract setup failed: {:#}", error);

                CfdEvent::ContractSetupFailed
            }
        };

        Ok(self.event(event))
    }

    pub fn roll_over(
        self,
        completed: RolloverCompleted,
    ) -> Result<Option<Event>, NoRolloverReason> {
        let event = match completed {
            Completed::Succeeded {
                payload: (dlc, funding_fee, _),
                ..
            } => {
                self.can_rollover()?;
                CfdEvent::RolloverCompleted { dlc, funding_fee }
            }
            Completed::Rejected { reason, .. } => {
                tracing::info!(order_id = %self.id, "Rollover was rejected: {:#}", reason);

                CfdEvent::RolloverRejected
            }
            Completed::Failed { error, .. } => {
                tracing::warn!(order_id = %self.id, "Rollover failed: {:#}", error);

                CfdEvent::RolloverFailed
            }
        };

        Ok(Some(self.event(event)))
    }

    pub fn settle_collaboratively(
        self,
        settlement: CollaborativeSettlementCompleted,
    ) -> Result<Event> {
        if !self.can_settle_collaboratively() {
            bail!("Cannot collaboratively settle")
        }

        let event = match settlement {
            Completed::Succeeded {
                payload: settlement,
                ..
            } => CfdEvent::CollaborativeSettlementCompleted {
                spend_tx: settlement.tx,
                script: settlement.script_pubkey,
                price: settlement.price,
            },
            Completed::Rejected { reason, .. } => {
                tracing::info!(order_id=%self.id(), "Collaborative close rejected: {:#}", reason);
                CfdEvent::CollaborativeSettlementRejected
            }
            Completed::Failed { error, .. } => {
                tracing::warn!(order_id=%self.id(), "Collaborative close failed: {:#}", error);
                CfdEvent::CollaborativeSettlementFailed
            }
        };

        Ok(self.event(event))
    }

    /// Given an attestation, find and decrypt the relevant CET.
    pub fn decrypt_cet(self, attestation: &oracle::Attestation) -> Result<Option<Event>> {
        if self.is_final() {
            return Ok(None);
        }

        let dlc = match self.dlc.as_ref() {
            Some(dlc) => dlc,
            None => return Ok(None),
        };

        let cet = dlc.signed_cet(attestation)?;

        let cet = match cet {
            Ok(cet) => cet,
            Err(IrrelevantAttestation { .. }) => {
                return Ok(None);
            }
        };

        let price = Price(Decimal::from(attestation.price));

        if self.cet_timelock_expired {
            return Ok(Some(
                self.event(CfdEvent::OracleAttestedPostCetTimelock { cet, price }),
            ));
        }

        // If we haven't yet emitted the commit tx, we need to emit it now.
        let commit_tx_to_emit = match self.commit_tx {
            Some(_) => None,
            None => Some(dlc.signed_commit_tx()?),
        };

        Ok(Some(self.event(CfdEvent::OracleAttestedPriorCetTimelock {
            timelocked_cet: cet,
            commit_tx: commit_tx_to_emit,
            price,
        })))
    }

    pub fn handle_cet_timelock_expired(mut self) -> Result<Event> {
        anyhow::ensure!(!self.is_final());

        let cfd_event = self
            .cet
            .take()
            // If we have cet, that means it has been attested
            .map(|cet| CfdEvent::CetTimelockExpiredPostOracleAttestation { cet })
            .unwrap_or_else(|| CfdEvent::CetTimelockExpiredPriorOracleAttestation);

        Ok(self.event(cfd_event))
    }

    pub fn handle_refund_timelock_expired(self) -> Result<Event, RefundTimelockExpiryError> {
        use RefundTimelockExpiryError::*;

        if self.is_final() {
            return Err(AlreadyFinal);
        }

        let dlc = self.dlc.as_ref().ok_or(NoDlc)?;
        let refund_tx = dlc.signed_refund_tx()?;

        let event = self.event(CfdEvent::RefundTimelockExpired { refund_tx });

        Ok(event)
    }

    pub fn handle_lock_confirmed(self) -> Event {
        // For the special case where we close when lock is still pending
        if self.is_final() {
            return self.event(CfdEvent::LockConfirmedAfterFinality);
        }

        self.event(CfdEvent::LockConfirmed)
    }

    pub fn handle_commit_confirmed(self) -> Event {
        self.event(CfdEvent::CommitConfirmed)
    }

    pub fn handle_collaborative_settlement_confirmed(self) -> Event {
        self.event(CfdEvent::CollaborativeSettlementConfirmed)
    }

    pub fn handle_cet_confirmed(self) -> Event {
        self.event(CfdEvent::CetConfirmed)
    }

    pub fn handle_refund_confirmed(self) -> Event {
        self.event(CfdEvent::RefundConfirmed)
    }

    pub fn handle_revoke_confirmed(self) -> Event {
        self.event(CfdEvent::RevokeConfirmed)
    }

    pub fn manual_commit_to_blockchain(&self) -> Result<Event> {
        anyhow::ensure!(!self.is_final());

        let dlc = self.dlc.as_ref().context("Cannot commit without a DLC")?;

        Ok(self.event(CfdEvent::ManualCommit {
            tx: dlc.signed_commit_tx()?,
        }))
    }

    fn event(&self, event: CfdEvent) -> Event {
        Event::new(self.id, event)
    }

    /// A factor to be added to the CFD order settlement_interval for calculating the
    /// refund timelock.
    ///
    /// The refund timelock is important in case the oracle disappears or never publishes a
    /// signature. Ideally, both users collaboratively settle in the refund scenario. This
    /// factor is important if the users do not settle collaboratively.
    /// `1.5` times the settlement_interval as defined in CFD order should be safe in the
    /// extreme case where a user publishes the commit transaction right after the contract was
    /// initialized. In this case, the oracle still has `1.0 *
    /// cfdorder.settlement_interval` time to attest and no one can publish the refund
    /// transaction.
    /// The downside is that if the oracle disappears: the users would only notice at the end
    /// of the cfd settlement_interval. In this case the users has to wait for another
    /// `1.5` times of the settlement_interval to get his funds back.
    const REFUND_THRESHOLD: f32 = 1.5;

    fn refund_timelock_in_blocks(&self) -> u32 {
        (self.settlement_interval * Self::REFUND_THRESHOLD)
            .as_blocks()
            .ceil() as u32
    }

    pub fn id(&self) -> OrderId {
        self.id
    }

    pub fn position(&self) -> Position {
        self.position
    }

    pub fn initial_price(&self) -> Price {
        self.initial_price
    }

    pub fn leverage(&self) -> Leverage {
        self.leverage
    }

    pub fn settlement_time_interval_hours(&self) -> Duration {
        self.settlement_interval
    }

    pub fn quantity(&self) -> Usd {
        self.quantity
    }

    pub fn counterparty_network_identity(&self) -> Identity {
        self.counterparty_network_identity
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn initial_funding_rate(&self) -> FundingRate {
        self.initial_funding_rate
    }

    pub fn initial_tx_fee_rate(&self) -> TxFeeRate {
        self.initial_tx_fee_rate
    }

    pub fn opening_fee(&self) -> OpeningFee {
        self.opening_fee
    }

    pub fn sign_collaborative_settlement_taker(
        &self,
        proposal: &SettlementProposal,
    ) -> Result<(Transaction, Signature, Script)> {
        let dlc = self
            .dlc
            .as_ref()
            .context("Collaborative close without DLC")?;

        let (tx, sig) = dlc.close_transaction(proposal)?;
        let script_pk = dlc.script_pubkey_for(Role::Taker);

        Ok((tx, sig, script_pk))
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn apply(mut self, evt: Event) -> Cfd {
        use CfdEvent::*;

        self.version += 1;

        match evt.event {
            ContractSetupStarted => self.during_contract_setup = true,
            ContractSetupCompleted { dlc } => {
                self.dlc = Some(dlc);
                self.during_contract_setup = false;
            }
            OracleAttestedPostCetTimelock { cet, .. } => self.cet = Some(cet),
            OracleAttestedPriorCetTimelock { timelocked_cet, .. } => {
                self.cet = Some(timelocked_cet);
            }
            ContractSetupFailed { .. } => {
                // TODO: Deal with failed contract setup
                self.during_contract_setup = false;
            }
            RolloverStarted => {
                self.during_rollover = true;
            }
            RolloverAccepted => {}
            RolloverCompleted { dlc, funding_fee } => {
                self.dlc = Some(dlc);
                self.during_rollover = false;
                self.fee_account = self.fee_account.add_funding_fee(funding_fee);
            }
            RolloverFailed { .. } => {
                self.during_rollover = false;
            }
            RolloverRejected => {
                self.during_rollover = false;
            }

            CollaborativeSettlementStarted { proposal } => {
                self.settlement_proposal = Some(proposal)
            }
            CollaborativeSettlementProposalAccepted { .. } => {}
            CollaborativeSettlementCompleted { spend_tx, .. } => {
                self.settlement_proposal = None;
                self.collaborative_settlement_spend_tx = Some(spend_tx);
            }
            CollaborativeSettlementRejected | CollaborativeSettlementFailed => {
                self.settlement_proposal = None;
            }
            CetConfirmed => self.cet_finality = true,
            RefundConfirmed => self.refund_finality = true,
            CollaborativeSettlementConfirmed => self.collaborative_settlement_finality = true,
            RefundTimelockExpired { .. } => self.refund_timelock_expired = true,
            LockConfirmed => self.lock_finality = true,
            LockConfirmedAfterFinality => self.lock_finality = true,
            CommitConfirmed => self.commit_finality = true,
            CetTimelockExpiredPriorOracleAttestation
            | CetTimelockExpiredPostOracleAttestation { .. } => {
                self.cet_timelock_expired = true;
            }
            OfferRejected => {
                // nothing to do here? A rejection means it should be impossible to issue any
                // commands
            }
            ManualCommit { tx } => self.commit_tx = Some(tx),
            RevokeConfirmed => {
                tracing::error!(order_id = %self.id, "Revoked logic not implemented");
                // TODO: we should punish the other party instead. For now, we pretend we are in
                // commit finalized and will receive our money based on an old CET.
                self.commit_finality = true;
            }
        }

        self
    }
}

pub trait AsBlocks {
    /// Calculates the duration in Bitcoin blocks.
    ///
    /// On Bitcoin there is a block every 10 minutes/600 seconds on average.
    /// It's the caller's responsibility to round the resulting floating point number.
    fn as_blocks(&self) -> f32;
}

impl AsBlocks for Duration {
    fn as_blocks(&self) -> f32 {
        self.as_seconds_f32() / 60.0 / 10.0
    }
}

// Make a `Margin` newtype and call `Margin::long`
/// Calculates the long's margin in BTC
///
/// The margin is the initial margin and represents the collateral the buyer
/// has to come up with to satisfy the contract. Here we calculate the initial
/// long margin as: quantity / (initial_price * leverage)
pub fn calculate_long_margin(price: Price, quantity: Usd, leverage: Leverage) -> Amount {
    quantity / (price * leverage)
}

/// Calculates the shorts's margin in BTC
///
/// The short margin is represented as the quantity of the contract given the
/// initial price. The short side can currently not leverage the position but
/// always has to cover the complete quantity.
pub fn calculate_short_margin(price: Price, quantity: Usd) -> Amount {
    quantity / price
}

pub fn calculate_long_liquidation_price(leverage: Leverage, price: Price) -> Price {
    price * leverage / (leverage + 1)
}

pub fn calculate_profit(payout: SignedAmount, margin: SignedAmount) -> (SignedAmount, Percent) {
    let profit = payout - margin;

    let profit_sats = Decimal::from(profit.as_sat());
    let margin_sats = Decimal::from(margin.as_sat());
    let percent = dec!(100) * profit_sats / margin_sats;

    (profit, Percent(percent))
}

/// Returns the profit/loss and payout capped by the provided margin
///
/// All values are calculated without using the payout curve.
/// Profit/loss is returned as signed bitcoin amount and percent.
pub fn calculate_profit_at_price(
    opening_price: Price,
    closing_price: Price,
    quantity: Usd,
    leverage: Leverage,
    fee_account: FeeAccount,
) -> Result<(SignedAmount, Percent, SignedAmount)> {
    let inv_initial_price =
        InversePrice::new(opening_price).context("cannot invert invalid price")?;
    let inv_closing_price =
        InversePrice::new(closing_price).context("cannot invert invalid price")?;
    let long_liquidation_price = calculate_long_liquidation_price(leverage, opening_price);
    let long_is_liquidated = closing_price <= long_liquidation_price;

    let long_margin = calculate_long_margin(opening_price, quantity, leverage)
        .to_signed()
        .context("Unable to compute long margin")?;
    let short_margin = calculate_short_margin(opening_price, quantity)
        .to_signed()
        .context("Unable to compute short margin")?;
    let amount_changed = (quantity * inv_initial_price)
        .to_signed()
        .context("Unable to convert to SignedAmount")?
        - (quantity * inv_closing_price)
            .to_signed()
            .context("Unable to convert to SignedAmount")?;

    // calculate profit/loss (P and L) in BTC
    let (margin, payout) = match fee_account.position {
        // TODO:
        // At this point, long_leverage == leverage, short_leverage == 1
        // which has the effect that the right boundary `b` below is
        // infinite and not used.
        //
        // The general case is:
        //   let:
        //     P = payout
        //     Q = quantity
        //     Ll = long_leverage
        //     Ls = short_leverage
        //     xi = initial_price
        //     xc = closing_price
        //
        //     a = xi * Ll / (Ll + 1)
        //     b = xi * Ls / (Ls - 1)
        //
        //     P_long(xc) = {
        //          0 if xc <= a,
        //          Q / (xi * Ll) + Q * (1 / xi - 1 / xc) if a < xc < b,
        //          Q / xi * (1/Ll + 1/Ls) if xc if xc >= b
        //     }
        //
        //     P_short(xc) = {
        //          Q / xi * (1/Ll + 1/Ls) if xc <= a,
        //          Q / (xi * Ls) - Q * (1 / xi - 1 / xc) if a < xc < b,
        //          0 if xc >= b
        //     }
        Position::Long => {
            let payout = match long_is_liquidated {
                true => SignedAmount::ZERO,
                false => long_margin + amount_changed - fee_account.balance(),
            };
            (long_margin, payout)
        }
        Position::Short => {
            let payout = match long_is_liquidated {
                true => long_margin + short_margin,
                false => short_margin - amount_changed - fee_account.balance(),
            };
            (short_margin, payout)
        }
    };

    let (profit_btc, profit_percent) = calculate_profit(payout, margin);
    Ok((profit_btc, profit_percent, payout))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cet {
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_sat")]
    pub maker_amount: Amount,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_sat")]
    pub taker_amount: Amount,
    pub adaptor_sig: EcdsaAdaptorSignature,

    // TODO: Range + number of digits (usize) could be represented as Digits similar to what we do
    // in the protocol lib
    pub range: RangeInclusive<u64>,
    pub n_bits: usize,

    pub txid: Txid,
}

impl Cet {
    /// Build an actual `Transaction` out of the payout information
    /// stored in `Self`, together with the input and the output
    /// addresses.
    ///
    /// The order of the outputs matters.
    ///
    /// We verify that the TXID of the resulting transaction matches
    /// the TXID with which `Self` was constructed.
    pub fn to_tx(
        &self,
        (commit_tx, commit_descriptor): (&Transaction, &Descriptor<PublicKey>),
        maker_address: &Address,
        taker_address: &Address,
    ) -> Result<Transaction> {
        let tx = Transaction {
            version: 2,
            input: vec![TxIn {
                previous_output: commit_tx.outpoint(&commit_descriptor.script_pubkey())?,
                sequence: CET_TIMELOCK,
                ..Default::default()
            }],
            lock_time: 0,
            output: vec![
                TxOut {
                    value: self.maker_amount.as_sat(),
                    script_pubkey: maker_address.script_pubkey(),
                },
                TxOut {
                    value: self.taker_amount.as_sat(),
                    script_pubkey: taker_address.script_pubkey(),
                },
            ],
        };

        if tx.txid() != self.txid {
            bail!("Reconstructed wrong CET");
        }

        Ok(tx)
    }
}

/// Contains all data we've assembled about the CFD through the setup protocol.
///
/// All contained signatures are the signatures of THE OTHER PARTY.
/// To use any of these transactions, we need to re-sign them with the correct secret key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Dlc {
    pub identity: SecretKey,
    pub identity_counterparty: PublicKey,
    pub revocation: SecretKey,
    pub revocation_pk_counterparty: PublicKey,
    pub publish: SecretKey,
    pub publish_pk_counterparty: PublicKey,
    pub maker_address: Address,
    pub taker_address: Address,

    /// The fully signed lock transaction ready to be published on chain
    pub lock: (Transaction, Descriptor<PublicKey>),
    pub commit: (Transaction, EcdsaAdaptorSignature, Descriptor<PublicKey>),
    pub cets: HashMap<BitMexPriceEventId, Vec<Cet>>,
    pub refund: (Transaction, Signature),

    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_sat")]
    pub maker_lock_amount: Amount,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_sat")]
    pub taker_lock_amount: Amount,

    pub revoked_commit: Vec<RevokedCommit>,

    // TODO: For now we store this seperately - it is a duplicate of what is stored in the cets
    // hashmap. The cet hashmap allows storing cets for event-ids with different concern
    // (settlement and liquidation-point). We should NOT make these fields public on the Dlc
    // and create an internal structure that depicts this properly and avoids duplication.
    pub settlement_event_id: BitMexPriceEventId,
    pub refund_timelock: u32,
}

impl Dlc {
    /// Create a close transaction based on the current contract and a settlement proposals
    pub fn close_transaction(
        &self,
        proposal: &crate::model::cfd::SettlementProposal,
    ) -> Result<(Transaction, Signature)> {
        let (lock_tx, lock_desc) = &self.lock;
        let (lock_outpoint, lock_amount) = {
            let outpoint = lock_tx
                .outpoint(&lock_desc.script_pubkey())
                .expect("lock script to be in lock tx");
            let amount = Amount::from_sat(lock_tx.output[outpoint.vout as usize].value);

            (outpoint, amount)
        };
        let (tx, sighash) = maia::close_transaction(
            lock_desc,
            lock_outpoint,
            lock_amount,
            (&self.maker_address, proposal.maker),
            (&self.taker_address, proposal.taker),
            1,
        )
        .context("Unable to collaborative close transaction")?;

        let sig = SECP256K1.sign(&sighash, &self.identity);

        Ok((tx, sig))
    }

    pub fn finalize_spend_transaction(
        &self,
        (close_tx, own_sig): (Transaction, Signature),
        counterparty_sig: Signature,
    ) -> Result<Transaction> {
        let own_pk = PublicKey::new(secp256k1_zkp::PublicKey::from_secret_key(
            SECP256K1,
            &self.identity,
        ));

        let (_, lock_desc) = &self.lock;
        let spend_tx = maia::finalize_spend_transaction(
            close_tx,
            lock_desc,
            (own_pk, own_sig),
            (self.identity_counterparty, counterparty_sig),
        )?;

        Ok(spend_tx)
    }

    pub fn refund_amount(&self, role: Role) -> Amount {
        let our_script_pubkey = match role {
            Role::Taker => self.taker_address.script_pubkey(),
            Role::Maker => self.maker_address.script_pubkey(),
        };

        self.refund
            .0
            .output
            .iter()
            .find(|output| output.script_pubkey == our_script_pubkey)
            .map(|output| Amount::from_sat(output.value))
            .unwrap_or_default()
    }

    pub fn script_pubkey_for(&self, role: Role) -> Script {
        match role {
            Role::Maker => self.maker_address.script_pubkey(),
            Role::Taker => self.taker_address.script_pubkey(),
        }
    }

    pub fn signed_refund_tx(&self) -> Result<Transaction> {
        let sig_hash = spending_tx_sighash(
            &self.refund.0,
            &self.commit.2,
            Amount::from_sat(self.commit.0.output[0].value),
        );
        let our_sig = SECP256K1.sign(&sig_hash, &self.identity);
        let our_pubkey = PublicKey::new(bdk::bitcoin::secp256k1::PublicKey::from_secret_key(
            SECP256K1,
            &self.identity,
        ));
        let counterparty_sig = self.refund.1;
        let counterparty_pubkey = self.identity_counterparty;
        let signed_refund_tx = finalize_spend_transaction(
            self.refund.0.clone(),
            &self.commit.2,
            (our_pubkey, our_sig),
            (counterparty_pubkey, counterparty_sig),
        )?;

        Ok(signed_refund_tx)
    }

    pub fn signed_commit_tx(&self) -> Result<Transaction> {
        let sig_hash = spending_tx_sighash(
            &self.commit.0,
            &self.lock.1,
            Amount::from_sat(self.lock.0.output[0].value),
        );
        let our_sig = SECP256K1.sign(&sig_hash, &self.identity);
        let our_pubkey = PublicKey::new(bdk::bitcoin::secp256k1::PublicKey::from_secret_key(
            SECP256K1,
            &self.identity,
        ));

        let counterparty_sig = self.commit.1.decrypt(&self.publish)?;
        let counterparty_pubkey = self.identity_counterparty;

        let signed_commit_tx = finalize_spend_transaction(
            self.commit.0.clone(),
            &self.lock.1,
            (our_pubkey, our_sig),
            (counterparty_pubkey, counterparty_sig),
        )?;

        Ok(signed_commit_tx)
    }

    pub fn signed_cet(
        &self,
        attestation: &oracle::Attestation,
    ) -> Result<Result<Transaction, IrrelevantAttestation>> {
        let cets = match self.cets.get(&attestation.id) {
            Some(cets) => cets,
            None => {
                return Ok(Err(IrrelevantAttestation {
                    id: attestation.id,
                    tx_id: self.lock.0.txid(),
                }))
            }
        };

        let cet = cets
            .iter()
            .find(|Cet { range, .. }| range.contains(&attestation.price))
            .context("Price out of range of cets")?;
        let encsig = cet.adaptor_sig;

        let mut decryption_sk = attestation.scalars[0];
        for oracle_attestation in attestation.scalars[1..cet.n_bits].iter() {
            decryption_sk.add_assign(oracle_attestation.as_ref())?;
        }

        let cet = cet
            .to_tx(
                (&self.commit.0, &self.commit.2),
                &self.maker_address,
                &self.taker_address,
            )
            .context("Failed to reconstruct CET")?;

        let sig_hash = spending_tx_sighash(
            &cet,
            &self.commit.2,
            Amount::from_sat(self.commit.0.output[0].value),
        );
        let our_sig = SECP256K1.sign(&sig_hash, &self.identity);
        let our_pubkey = PublicKey::new(bdk::bitcoin::secp256k1::PublicKey::from_secret_key(
            SECP256K1,
            &self.identity,
        ));

        let counterparty_sig = encsig.decrypt(&decryption_sk)?;
        let counterparty_pubkey = self.identity_counterparty;

        let signed_cet = finalize_spend_transaction(
            cet,
            &self.commit.2,
            (our_pubkey, our_sig),
            (counterparty_pubkey, counterparty_sig),
        )?;

        Ok(Ok(signed_cet))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Attestation {id} is irrelevant for DLC {tx_id}")]
pub struct IrrelevantAttestation {
    id: BitMexPriceEventId,
    tx_id: Txid,
}

/// Information which we need to remember in order to construct a
/// punishment transaction in case the counterparty publishes a
/// revoked commit transaction.
///
/// It also includes the information needed to monitor for the
/// publication of the revoked commit transaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokedCommit {
    // To build punish transaction
    pub encsig_ours: EcdsaAdaptorSignature,
    pub revocation_sk_theirs: SecretKey,
    pub publication_pk_theirs: PublicKey,
    // To monitor revoked commit transaction
    pub txid: Txid,
    pub script_pubkey: Script,
}

/// Used when transactions (e.g. collaborative close) are recorded as a part of
/// CfdState in the cases when we can't solely rely on state transition
/// timestamp as it could have occured for different reasons (like a new
/// attestation in Open state)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CollaborativeSettlement {
    pub tx: Transaction,
    pub script_pubkey: Script,
    pub timestamp: Timestamp,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_sat")]
    payout: Amount,
    price: Price,
}

impl CollaborativeSettlement {
    pub fn new(tx: Transaction, own_script_pubkey: Script, price: Price) -> Result<Self> {
        // Falls back to Amount::ZERO in case we don't find an output that matches out script pubkey
        // The assumption is, that this can happen for cases where we were liquidated
        let payout = match tx
            .output
            .iter()
            .find(|output| output.script_pubkey == own_script_pubkey)
            .map(|output| Amount::from_sat(output.value))
        {
            Some(payout) => payout,
            None => {
                tracing::error!(
                    "Collaborative settlement with a zero amount, this should really not happen!"
                );
                Amount::ZERO
            }
        };

        Ok(Self {
            tx,
            script_pubkey: own_script_pubkey,
            timestamp: Timestamp::now(),
            payout,
            price,
        })
    }

    pub fn payout(&self) -> Amount {
        self.payout
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Completed<P, E> {
    Succeeded {
        order_id: OrderId,
        payload: P,
    },
    Rejected {
        order_id: OrderId,
        reason: anyhow::Error,
    },
    Failed {
        order_id: OrderId,
        error: E,
    },
}

impl<P, E> xtra::Message for Completed<P, E>
where
    P: Send + 'static,
    E: Send + 'static,
{
    type Result = Result<()>;
}

impl<P, E> Completed<P, E> {
    pub fn order_id(&self) -> OrderId {
        *match self {
            Completed::Succeeded { order_id, .. } => order_id,
            Completed::Rejected { order_id, .. } => order_id,
            Completed::Failed { order_id, .. } => order_id,
        }
    }

    pub fn rejected(order_id: OrderId) -> Self {
        Self::Rejected {
            order_id,
            reason: anyhow::format_err!("unknown"),
        }
    }
    pub fn rejected_due_to(order_id: OrderId, reason: anyhow::Error) -> Self {
        Self::Rejected { order_id, reason }
    }

    pub fn failed(order_id: OrderId, error: E) -> Self {
        Self::Failed { order_id, error }
    }
}

pub mod marker {
    /// Marker type for contract setup completion
    #[derive(Debug)]
    pub struct Setup;
    /// Marker type for rollover  completion
    #[derive(Debug)]
    pub struct Rollover;
}

/// Message sent from a setup actor to the
/// cfd actor to notify that the contract setup has finished.
pub type SetupCompleted = Completed<(Dlc, marker::Setup), anyhow::Error>;

/// Message sent from a rollover actor to the
/// cfd actor to notify that the rollover has finished (contract got updated).
pub type RolloverCompleted = Completed<(Dlc, FundingFee, marker::Rollover), anyhow::Error>;

pub type CollaborativeSettlementCompleted = Completed<CollaborativeSettlement, anyhow::Error>;

impl Completed<(Dlc, marker::Setup), anyhow::Error> {
    pub fn succeeded(order_id: OrderId, dlc: Dlc) -> Self {
        Self::Succeeded {
            order_id,
            payload: (dlc, marker::Setup),
        }
    }
}

impl Completed<(Dlc, FundingFee, marker::Rollover), anyhow::Error> {
    pub fn succeeded(order_id: OrderId, dlc: Dlc, funding_fee: FundingFee) -> Self {
        Self::Succeeded {
            order_id,
            payload: (dlc, funding_fee, marker::Rollover),
        }
    }
}

mod hex_transaction {
    use super::*;
    use bdk::bitcoin;
    use serde::Deserializer;
    use serde::Serializer;

    pub fn serialize<S: Serializer>(value: &Transaction, serializer: S) -> Result<S::Ok, S::Error> {
        let bytes = bitcoin::consensus::serialize(value);
        let hex_str = hex::encode(bytes);
        serializer.serialize_str(hex_str.as_str())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Transaction, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex = String::deserialize(deserializer).map_err(D::Error::custom)?;
        let bytes = hex::decode(hex).map_err(D::Error::custom)?;
        let tx = bitcoin::consensus::deserialize(&bytes).map_err(D::Error::custom)?;
        Ok(tx)
    }

    pub mod opt {
        use super::*;

        pub fn serialize<S: Serializer>(
            value: &Option<Transaction>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            match value {
                None => serializer.serialize_none(),
                Some(value) => super::serialize(value, serializer),
            }
        }

        pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Transaction>, D::Error>
        where
            D: Deserializer<'de>,
        {
            let hex = match Option::<String>::deserialize(deserializer).map_err(D::Error::custom)? {
                None => return Ok(None),
                Some(hex) => hex,
            };

            let bytes = hex::decode(hex).map_err(D::Error::custom)?;
            let tx = bitcoin::consensus::deserialize(&bytes).map_err(D::Error::custom)?;

            Ok(Some(tx))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed::RandomSeed;
    use crate::seed::Seed;
    use bdk::bitcoin::util::psbt::Global;
    use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;
    use std::str::FromStr;
    use time::macros::datetime;

    #[test]
    fn given_default_values_then_expected_liquidation_price() {
        let price = Price::new(dec!(46125)).unwrap();
        let leverage = Leverage::new(5).unwrap();
        let expected = Price::new(dec!(38437.5)).unwrap();

        let liquidation_price = calculate_long_liquidation_price(leverage, price);

        assert_eq!(liquidation_price, expected);
    }

    #[test]
    fn given_leverage_of_one_and_equal_price_and_quantity_then_long_margin_is_one_btc() {
        let price = Price::new(dec!(40000)).unwrap();
        let quantity = Usd::new(dec!(40000));
        let leverage = Leverage::new(1).unwrap();

        let long_margin = calculate_long_margin(price, quantity, leverage);

        assert_eq!(long_margin, Amount::ONE_BTC);
    }

    #[test]
    fn given_leverage_of_one_and_leverage_of_ten_then_long_margin_is_lower_factor_ten() {
        let price = Price::new(dec!(40000)).unwrap();
        let quantity = Usd::new(dec!(40000));
        let leverage = Leverage::new(10).unwrap();

        let long_margin = calculate_long_margin(price, quantity, leverage);

        assert_eq!(long_margin, Amount::from_btc(0.1).unwrap());
    }

    #[test]
    fn given_quantity_equals_price_then_short_margin_is_one_btc() {
        let price = Price::new(dec!(40000)).unwrap();
        let quantity = Usd::new(dec!(40000));

        let short_margin = calculate_short_margin(price, quantity);

        assert_eq!(short_margin, Amount::ONE_BTC);
    }

    #[test]
    fn given_quantity_half_of_price_then_short_margin_is_half_btc() {
        let price = Price::new(dec!(40000)).unwrap();
        let quantity = Usd::new(dec!(20000));

        let short_margin = calculate_short_margin(price, quantity);

        assert_eq!(short_margin, Amount::from_btc(0.5).unwrap());
    }

    #[test]
    fn given_quantity_double_of_price_then_short_margin_is_two_btc() {
        let price = Price::new(dec!(40000)).unwrap();
        let quantity = Usd::new(dec!(80000));

        let short_margin = calculate_short_margin(price, quantity);

        assert_eq!(short_margin, Amount::from_btc(2.0).unwrap());
    }

    #[test]
    fn test_secs_into_blocks() {
        let error_margin = f32::EPSILON;

        let duration = Duration::seconds(600);
        let blocks = duration.as_blocks();
        assert!(blocks - error_margin < 1.0 && blocks + error_margin > 1.0);

        let duration = Duration::seconds(0);
        let blocks = duration.as_blocks();
        assert!(blocks - error_margin < 0.0 && blocks + error_margin > 0.0);

        let duration = Duration::seconds(60);
        let blocks = duration.as_blocks();
        assert!(blocks - error_margin < 0.1 && blocks + error_margin > 0.1);
    }

    #[test]
    fn calculate_profit_and_loss() {
        let empty_fee_long = FeeAccount::new(Position::Long, Role::Taker);
        let empty_fee_short = FeeAccount::new(Position::Short, Role::Maker);

        assert_profit_loss_values(
            Price::new(dec!(10_000)).unwrap(),
            Price::new(dec!(10_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::new(2).unwrap(),
            empty_fee_long,
            SignedAmount::ZERO,
            Decimal::ZERO.into(),
            "No price increase means no profit",
        );

        assert_profit_loss_values(
            Price::new(dec!(10_000)).unwrap(),
            Price::new(dec!(10_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::new(2).unwrap(),
            empty_fee_long.add_funding_fee(FundingFee::new(
                Amount::from_sat(500),
                FundingRate::new(dec!(0.001)).unwrap(),
            )),
            SignedAmount::from_sat(-500),
            dec!(-0.001).into(),
            "No price increase but fee means fee",
        );

        assert_profit_loss_values(
            Price::new(dec!(10_000)).unwrap(),
            Price::new(dec!(10_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::new(2).unwrap(),
            empty_fee_short.add_funding_fee(FundingFee::new(
                Amount::from_sat(500),
                FundingRate::new(dec!(0.001)).unwrap(),
            )),
            SignedAmount::from_sat(500),
            dec!(0.0005).into(),
            "No price increase but fee means fee",
        );

        assert_profit_loss_values(
            Price::new(dec!(10_000)).unwrap(),
            Price::new(dec!(20_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::new(2).unwrap(),
            empty_fee_long,
            SignedAmount::from_sat(50_000_000),
            dec!(100).into(),
            "A price increase of 2x should result in a profit of 100% (long)",
        );

        assert_profit_loss_values(
            Price::new(dec!(9_000)).unwrap(),
            Price::new(dec!(6_000)).unwrap(),
            Usd::new(dec!(9_000)),
            Leverage::new(2).unwrap(),
            empty_fee_long,
            SignedAmount::from_sat(-50_000_000),
            dec!(-100).into(),
            "A price drop of 1/(Leverage + 1) x should result in 100% loss (long)",
        );

        assert_profit_loss_values(
            Price::new(dec!(10_000)).unwrap(),
            Price::new(dec!(5_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::new(2).unwrap(),
            empty_fee_long,
            SignedAmount::from_sat(-50_000_000),
            dec!(-100).into(),
            "A loss should be capped at 100% (long)",
        );

        assert_profit_loss_values(
            Price::new(dec!(50_400)).unwrap(),
            Price::new(dec!(60_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::new(2).unwrap(),
            empty_fee_long,
            SignedAmount::from_sat(3_174_603),
            dec!(31.999997984000016127999870976).into(),
            "long position should make a profit when price goes up",
        );

        assert_profit_loss_values(
            Price::new(dec!(50_400)).unwrap(),
            Price::new(dec!(60_000)).unwrap(),
            Usd::new(dec!(10_000)),
            Leverage::new(2).unwrap(),
            empty_fee_short,
            SignedAmount::from_sat(-3_174_603),
            dec!(-15.999998992000008063999935488).into(),
            "short position should make a loss when price goes up",
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn assert_profit_loss_values(
        initial_price: Price,
        closing_price: Price,
        quantity: Usd,
        leverage: Leverage,
        fee_account: FeeAccount,
        should_profit: SignedAmount,
        should_profit_in_percent: Percent,
        msg: &str,
    ) {
        // TODO: Assert on payout as well

        let (profit, in_percent, _) = calculate_profit_at_price(
            initial_price,
            closing_price,
            quantity,
            leverage,
            fee_account,
        )
        .unwrap();

        assert_eq!(profit, should_profit, "{}", msg);
        assert_eq!(in_percent, should_profit_in_percent, "{}", msg);
    }

    #[test]
    fn test_profit_calculation_loss_plus_profit_should_be_zero() {
        let initial_price = Price::new(dec!(10_000)).unwrap();
        let closing_price = Price::new(dec!(16_000)).unwrap();
        let quantity = Usd::new(dec!(10_000));
        let leverage = Leverage::new(1).unwrap();

        let opening_fee = OpeningFee::new(Amount::from_sat(500));
        let funding_fee = FundingFee::new(
            Amount::from_sat(100),
            FundingRate::new(dec!(0.001)).unwrap(),
        );

        let taker_long = FeeAccount::new(Position::Long, Role::Taker)
            .add_opening_fee(opening_fee)
            .add_funding_fee(funding_fee);

        let maker_short = FeeAccount::new(Position::Short, Role::Maker)
            .add_opening_fee(opening_fee)
            .add_funding_fee(funding_fee);

        let (profit, profit_in_percent, _) =
            calculate_profit_at_price(initial_price, closing_price, quantity, leverage, taker_long)
                .unwrap();
        let (loss, loss_in_percent, _) = calculate_profit_at_price(
            initial_price,
            closing_price,
            quantity,
            leverage,
            maker_short,
        )
        .unwrap();

        assert_eq!(profit.checked_add(loss).unwrap(), SignedAmount::ZERO);
        // NOTE:
        // this is only true when long_leverage == short_leverage
        assert_eq!(
            profit_in_percent.0.checked_add(loss_in_percent.0).unwrap(),
            Decimal::ZERO
        );
    }

    #[test]
    fn margin_remains_constant() {
        let initial_price = Price::new(dec!(15_000)).unwrap();
        let quantity = Usd::new(dec!(10_000));
        let leverage = Leverage::new(2).unwrap();
        let long_margin = calculate_long_margin(initial_price, quantity, leverage)
            .to_signed()
            .unwrap();
        let short_margin = calculate_short_margin(initial_price, quantity)
            .to_signed()
            .unwrap();
        let pool_amount = SignedAmount::ONE_BTC;
        let closing_prices = [
            Price::new(dec!(0.15)).unwrap(),
            Price::new(dec!(1.5)).unwrap(),
            Price::new(dec!(15)).unwrap(),
            Price::new(dec!(150)).unwrap(),
            Price::new(dec!(1_500)).unwrap(),
            Price::new(dec!(15_000)).unwrap(),
            Price::new(dec!(150_000)).unwrap(),
            Price::new(dec!(1_500_000)).unwrap(),
            Price::new(dec!(15_000_000)).unwrap(),
        ];

        let opening_fee = OpeningFee::new(Amount::from_sat(500));
        let funding_fee = FundingFee::new(
            Amount::from_sat(100),
            FundingRate::new(dec!(0.001)).unwrap(),
        );

        let taker_long = FeeAccount::new(Position::Long, Role::Taker)
            .add_opening_fee(opening_fee)
            .add_funding_fee(funding_fee);

        let maker_short = FeeAccount::new(Position::Short, Role::Maker)
            .add_opening_fee(opening_fee)
            .add_funding_fee(funding_fee);

        for price in closing_prices {
            let (long_profit, _, _) =
                calculate_profit_at_price(initial_price, price, quantity, leverage, taker_long)
                    .unwrap();
            let (short_profit, _, _) =
                calculate_profit_at_price(initial_price, price, quantity, leverage, maker_short)
                    .unwrap();

            assert_eq!(
                long_profit + long_margin + short_profit + short_margin,
                pool_amount
            );
        }
    }

    #[test]
    fn order_id_serde_roundtrip() {
        let id = OrderId::default();

        let deserialized = serde_json::from_str(&serde_json::to_string(&id).unwrap()).unwrap();

        assert_eq!(id, deserialized);
    }

    #[test]
    fn cfd_event_to_json() {
        let event = CfdEvent::ContractSetupFailed;

        let (name, data) = event.to_json();

        assert_eq!(name, "ContractSetupFailed");
        assert_eq!(data, r#"null"#);
    }

    #[test]
    fn cfd_event_from_json() {
        let name = "ContractSetupFailed".to_owned();
        let data = r#"null"#.to_owned();

        let event = CfdEvent::from_json(name, data).unwrap();

        assert_eq!(event, CfdEvent::ContractSetupFailed);
    }

    #[test]
    fn cfd_ensure_stable_names_for_expensive_events() {
        let (rollover_event_name, _) = CfdEvent::RolloverCompleted {
            dlc: Dlc::dummy(None),
            funding_fee: FundingFee::new(Amount::ZERO, FundingRate::default()),
        }
        .to_json();

        let (setup_event_name, _) = CfdEvent::ContractSetupCompleted {
            dlc: Dlc::dummy(None),
        }
        .to_json();

        assert_eq!(setup_event_name, CONTRACT_SETUP_COMPLETED_EVENT.to_owned());
        assert_eq!(rollover_event_name, ROLLOVER_COMPLETED_EVENT.to_owned());
    }

    #[test]
    fn cfd_event_no_data_from_json() {
        let name = "OfferRejected".to_owned();
        let data = r#"null"#.to_owned();

        let event = CfdEvent::from_json(name, data).unwrap();

        assert_eq!(event, CfdEvent::OfferRejected);
    }

    #[test]
    fn given_cfd_expires_now_then_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //                                                          now

        let cfd = Cfd::dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));
        let result = cfd.can_auto_rollover_taker(datetime!(2021-11-19 10:00:00).assume_utc());

        assert!(result.is_ok());
    }

    #[test]
    fn given_cfd_expires_within_23hours_then_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //        now

        let cfd = Cfd::dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));

        let result = cfd.can_auto_rollover_taker(datetime!(2021-11-18 11:00:00).assume_utc());

        assert!(result.is_ok());
    }

    #[test]
    fn given_cfd_was_just_rolled_over_then_no_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //    now

        let cfd = Cfd::dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));
        let cannot_roll_over = cfd
            .can_auto_rollover_taker(datetime!(2021-11-18 10:00:01).assume_utc())
            .unwrap_err();

        assert_eq!(cannot_roll_over, NoRolloverReason::TooRecent)
    }

    #[test]
    fn given_cfd_out_of_bounds_expiry_then_no_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //  now

        let cfd = Cfd::dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));
        let cannot_roll_over = cfd
            .can_auto_rollover_taker(datetime!(2021-11-18 09:59:59).assume_utc())
            .unwrap_err();

        assert_eq!(cannot_roll_over, NoRolloverReason::TooRecent)
    }

    #[test]
    fn given_cfd_was_renewed_less_than_1h_ago_then_no_rollover() {
        // --|----|-------------------------------------------------|--> time
        //   ct   1h                                                24h
        // --|----|<--------------------rollover------------------->|--
        //       now

        let cfd = Cfd::dummy_open(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));
        let cannot_roll_over = cfd
            .can_auto_rollover_taker(datetime!(2021-11-18 10:59:59).assume_utc())
            .unwrap_err();

        assert_eq!(cannot_roll_over, NoRolloverReason::TooRecent)
    }

    #[test]
    fn given_cfd_not_locked_then_no_rollover() {
        let cfd = Cfd::dummy_not_open_yet();

        let cannot_roll_over = cfd.can_rollover().unwrap_err();

        assert!(matches!(
            cannot_roll_over,
            NoRolloverReason::NotLocked { .. }
        ))
    }

    #[test]
    fn given_cfd_has_attestation_then_no_rollover() {
        let cfd = Cfd::dummy_with_attestation(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));

        let cannot_roll_over = cfd.can_rollover().unwrap_err();

        assert!(matches!(
            cannot_roll_over,
            NoRolloverReason::Attested { .. }
        ))
    }

    #[test]
    fn given_cfd_final_then_no_rollover() {
        let cfd = Cfd::dummy_final(BitMexPriceEventId::with_20_digits(
            datetime!(2021-11-19 10:00:00).assume_utc(),
        ));

        let cannot_roll_over = cfd.can_rollover().unwrap_err();

        assert!(matches!(cannot_roll_over, NoRolloverReason::Final))
    }

    #[test]
    fn can_calculate_funding_fee_with_negative_funding_rate() {
        let funding_rate = FundingRate::new(Decimal::NEGATIVE_ONE).unwrap();
        let funding_fee = calculate_funding_fee(
            Price::new(dec!(1)).unwrap(),
            Usd::new(dec!(1)),
            Leverage::new(1).unwrap(),
            funding_rate,
            SETTLEMENT_INTERVAL.whole_hours(),
        )
        .unwrap();

        assert_eq!(funding_fee.fee, Amount::ONE_BTC);
        assert_eq!(funding_fee.rate, funding_rate);
    }

    use proptest::prelude::*;
    proptest! {
        #[test]
        fn rollover_funding_fee_collected_incrementally_should_not_be_smaller_than_collected_once_per_settlement_interval(quantity in 1u64..100_000u64) {
            let funding_rate = FundingRate::new(dec!(0.01)).unwrap();
            let price = Price::new(dec!(10_000)).unwrap();
            let quantity = Usd::new(Decimal::from(quantity));
            let leverage = Leverage::new(1).unwrap();

            let funding_fee_for_whole_interval =
                calculate_funding_fee(
                    price,
                    quantity, leverage , funding_rate, SETTLEMENT_INTERVAL.whole_hours()).unwrap();
            let funding_fee_for_one_hour =
                calculate_funding_fee(price, quantity, leverage, funding_rate, 1).unwrap();
            let fee_account = FeeAccount::new(Position::Long, Role::Taker);

            let fee_account_whole_interval = fee_account.add_funding_fee(funding_fee_for_whole_interval);
            let fee_account_one_hour = fee_account.add_funding_fee(funding_fee_for_one_hour);

            let total_balance_when_collected_hourly = fee_account_one_hour.balance().checked_mul(SETTLEMENT_INTERVAL.whole_hours()).unwrap();
            let total_balance_when_collected_for_whole_interval = fee_account_whole_interval.balance();

            prop_assert!(
                total_balance_when_collected_hourly >= total_balance_when_collected_for_whole_interval,
                "when charged per hour we should not be at loss as compared to charging once per settlement interval"
            );

            prop_assert!(
            total_balance_when_collected_hourly - total_balance_when_collected_for_whole_interval < SignedAmount::from_sat(30), "we should not overcharge"
       );
    }
    }

    impl Event {
        fn dummy_open(event_id: BitMexPriceEventId) -> Vec<Self> {
            vec![
                Event {
                    timestamp: Timestamp::now(),
                    id: Default::default(),
                    event: CfdEvent::ContractSetupStarted,
                },
                Event {
                    timestamp: Timestamp::now(),
                    id: Default::default(),
                    event: CfdEvent::ContractSetupCompleted {
                        dlc: Dlc::dummy(Some(event_id)),
                    },
                },
                Event {
                    timestamp: Timestamp::now(),
                    id: Default::default(),
                    event: CfdEvent::LockConfirmed,
                },
            ]
        }

        fn dummy_attestation_prior_timelock(event_id: BitMexPriceEventId) -> Vec<Self> {
            let mut open = Self::dummy_open(event_id);
            open.push(Event {
                timestamp: Timestamp::now(),
                id: Default::default(),
                event: CfdEvent::OracleAttestedPriorCetTimelock {
                    timelocked_cet: dummy_transaction(),
                    commit_tx: Some(dummy_transaction()),
                    price: Price(dec!(10000)),
                },
            });

            open
        }

        fn dummy_final_cet(event_id: BitMexPriceEventId) -> Vec<Self> {
            let mut open = Self::dummy_open(event_id);
            open.push(Event {
                timestamp: Timestamp::now(),
                id: Default::default(),
                event: CfdEvent::CetConfirmed,
            });

            open
        }
    }

    impl Cfd {
        fn dummy_not_open_yet() -> Self {
            Cfd::from_order(
                Order::dummy_model(),
                Position::Long,
                Usd::new(dec!(1000)),
                dummy_identity(),
                Role::Taker,
            )
        }

        fn dummy_open(event_id: BitMexPriceEventId) -> Self {
            let cfd = Cfd::from_order(
                Order::dummy_model(),
                Position::Long,
                Usd::new(dec!(1000)),
                dummy_identity(),
                Role::Taker,
            );

            Event::dummy_open(event_id)
                .into_iter()
                .fold(cfd, Cfd::apply)
        }

        fn dummy_with_attestation(event_id: BitMexPriceEventId) -> Self {
            let cfd = Cfd::from_order(
                Order::dummy_model(),
                Position::Long,
                Usd::new(dec!(1000)),
                dummy_identity(),
                Role::Taker,
            );

            Event::dummy_attestation_prior_timelock(event_id)
                .into_iter()
                .fold(cfd, Cfd::apply)
        }

        fn dummy_final(event_id: BitMexPriceEventId) -> Self {
            let cfd = Cfd::from_order(
                Order::dummy_model(),
                Position::Long,
                Usd::new(dec!(1000)),
                dummy_identity(),
                Role::Taker,
            );

            Event::dummy_final_cet(event_id)
                .into_iter()
                .fold(cfd, Cfd::apply)
        }
    }

    impl Order {
        fn dummy_model() -> Self {
            Order::new_short(
                Price::new(dec!(1000)).unwrap(),
                Usd::new(dec!(100)),
                Usd::new(dec!(1000)),
                Origin::Ours,
                dummy_event_id(),
                time::Duration::hours(24),
                TxFeeRate::default(),
                FundingRate::default(),
                OpeningFee::default(),
            )
            .unwrap()
        }
    }

    impl Dlc {
        fn dummy(event_id: Option<BitMexPriceEventId>) -> Self {
            let dummy_sk = SecretKey::from_slice(&[1; 32]).unwrap();
            let dummy_pk = PublicKey::from_slice(&[
                3, 23, 183, 225, 206, 31, 159, 148, 195, 42, 67, 115, 146, 41, 248, 140, 11, 3, 51,
                41, 111, 180, 110, 143, 114, 134, 88, 73, 198, 174, 52, 184, 78,
            ])
            .unwrap();

            let dummy_addr = Address::from_str("132F25rTsvBdp9JzLLBHP5mvGY66i1xdiM").unwrap();

            let dummy_tx = dummy_partially_signed_transaction().extract_tx();
            let dummy_adapter_sig = "03424d14a5471c048ab87b3b83f6085d125d5864249ae4297a57c84e74710bb6730223f325042fce535d040fee52ec13231bf709ccd84233c6944b90317e62528b2527dff9d659a96db4c99f9750168308633c1867b70f3a18fb0f4539a1aecedcd1fc0148fc22f36b6303083ece3f872b18e35d368b3958efe5fb081f7716736ccb598d269aa3084d57e1855e1ea9a45efc10463bbf32ae378029f5763ceb40173f"
                .parse()
                .unwrap();

            let dummy_sig = Signature::from_str("3046022100839c1fbc5304de944f697c9f4b1d01d1faeba32d751c0f7acb21ac8a0f436a72022100e89bd46bb3a5a62adc679f659b7ce876d83ee297c7a5587b2011c4fcc72eab45").unwrap();

            let mut dummy_cet_with_zero_price_range = HashMap::new();
            dummy_cet_with_zero_price_range.insert(
                BitMexPriceEventId::with_20_digits(OffsetDateTime::now_utc()),
                vec![Cet {
                    maker_amount: Amount::from_sat(0),
                    taker_amount: Amount::from_sat(0),
                    adaptor_sig: dummy_adapter_sig,
                    range: RangeInclusive::new(0, 1),
                    n_bits: 0,
                    txid: dummy_tx.txid(),
                }],
            );

            Dlc {
                identity: dummy_sk,
                identity_counterparty: dummy_pk,
                revocation: dummy_sk,
                revocation_pk_counterparty: dummy_pk,
                publish: dummy_sk,
                publish_pk_counterparty: dummy_pk,
                maker_address: dummy_addr.clone(),
                taker_address: dummy_addr,
                lock: (dummy_tx.clone(), Descriptor::new_pk(dummy_pk)),
                commit: (
                    dummy_tx.clone(),
                    dummy_adapter_sig,
                    Descriptor::new_pk(dummy_pk),
                ),
                cets: dummy_cet_with_zero_price_range,
                refund: (dummy_tx, dummy_sig),
                maker_lock_amount: Default::default(),
                taker_lock_amount: Default::default(),
                revoked_commit: vec![],
                settlement_event_id: match event_id {
                    Some(event_id) => event_id,
                    None => dummy_event_id(),
                },
                refund_timelock: 0,
            }
        }
    }

    pub fn dummy_transaction() -> Transaction {
        dummy_partially_signed_transaction().extract_tx()
    }

    pub fn dummy_partially_signed_transaction() -> PartiallySignedTransaction {
        // very simple dummy psbt that does not contain anything
        // pulled in from github.com-1ecc6299db9ec823/bitcoin-0.27.1/src/util/psbt/mod.rs:238

        PartiallySignedTransaction {
            global: Global {
                unsigned_tx: Transaction {
                    version: 2,
                    lock_time: 0,
                    input: vec![],
                    output: vec![],
                },
                xpub: Default::default(),
                version: 0,
                proprietary: BTreeMap::new(),
                unknown: BTreeMap::new(),
            },
            inputs: vec![],
            outputs: vec![],
        }
    }

    pub fn dummy_identity() -> Identity {
        Identity::new(RandomSeed::default().derive_identity().0)
    }

    pub fn dummy_event_id() -> BitMexPriceEventId {
        BitMexPriceEventId::with_20_digits(OffsetDateTime::now_utc())
    }
}
