use crate::model::{Leverage, OracleEventId, Percent, Position, TakerId, TradingPair, Usd};
use crate::{monitor, oracle};
use anyhow::{bail, Context, Result};
use bdk::bitcoin::secp256k1::{SecretKey, Signature};
use bdk::bitcoin::{Address, Amount, PublicKey, Script, SignedAmount, Transaction, Txid};
use bdk::descriptor::Descriptor;
use cfd_protocol::secp256k1_zkp::{EcdsaAdaptorSignature, SECP256K1};
use cfd_protocol::{finalize_spend_transaction, spending_tx_sighash};
use rocket::request::FromParam;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::ops::{Neg, RangeInclusive};
use std::time::SystemTime;
use time::Duration;
use uuid::Uuid;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct OrderId(Uuid);

impl Default for OrderId {
    fn default() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<'v> FromParam<'v> for OrderId {
    type Error = uuid::Error;

    fn from_param(param: &'v str) -> Result<Self, Self::Error> {
        let uuid = param.parse::<Uuid>()?;
        Ok(OrderId(uuid))
    }
}

// TODO: Could potentially remove this and use the Role in the Order instead
/// Origin of the order
#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
pub enum Origin {
    Ours,
    Theirs,
}

/// Role in the Cfd
#[derive(Debug, Copy, Clone, PartialEq)]
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

    pub price: Usd,

    // TODO: [post-MVP] Representation of the contract size; at the moment the contract size is
    //  always 1 USD
    pub min_quantity: Usd,
    pub max_quantity: Usd,

    // TODO: [post-MVP] - Once we have multiple leverage we will have to move leverage and
    //  liquidation_price into the CFD and add a calculation endpoint for the taker buy screen
    pub leverage: Leverage,
    pub liquidation_price: Usd,

    pub creation_timestamp: SystemTime,

    /// The duration that will be used for calculating the settlement timestamp
    pub term: Duration,

    pub origin: Origin,

    /// The id of the event to be used for price attestation
    ///
    /// The maker includes this into the Order based on the Oracle announcement to be used.
    pub oracle_event_id: OracleEventId,
}

#[allow(dead_code)] // Only one binary and the tests use this.
impl Order {
    pub const TERM: Duration = Duration::hours(24);

    pub fn new(
        price: Usd,
        min_quantity: Usd,
        max_quantity: Usd,
        origin: Origin,
        oracle_event_id: OracleEventId,
    ) -> Result<Self> {
        let leverage = Leverage(2);
        let maintenance_margin_rate = dec!(0.005);
        let liquidation_price =
            calculate_liquidation_price(&leverage, &price, &maintenance_margin_rate)?;

        Ok(Order {
            id: OrderId::default(),
            price,
            min_quantity,
            max_quantity,
            leverage,
            trading_pair: TradingPair::BtcUsd,
            liquidation_price,
            position: Position::Sell,
            creation_timestamp: SystemTime::now(),
            term: Self::TERM,
            origin,
            oracle_event_id,
        })
    }
}

fn calculate_liquidation_price(
    leverage: &Leverage,
    price: &Usd,
    maintenance_margin_rate: &Decimal,
) -> Result<Usd> {
    let leverage = Decimal::from(leverage.0).into();
    let maintenance_margin_rate: Usd = (*maintenance_margin_rate).into();

    // liquidation price calc in isolated margin mode
    // currently based on: https://help.bybit.com/hc/en-us/articles/360039261334-How-to-calculate-Liquidation-Price-Inverse-Contract-
    let liquidation_price = price.checked_mul(leverage)?.checked_div(
        leverage
            .checked_add(Decimal::ONE.into())?
            .checked_sub(maintenance_margin_rate.checked_mul(leverage)?)?,
    )?;

    Ok(liquidation_price)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Error {
    Connect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CfdStateError {
    last_successful_state: CfdState,
    error: Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct CfdStateCommon {
    pub transition_timestamp: SystemTime,
}

// Note: De-/Serialize with type tag to make handling on UI easier
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum CfdState {
    /// The taker sent an order to the maker to open the CFD but doesn't have a response yet.
    ///
    /// This state applies to taker only.
    OutgoingOrderRequest { common: CfdStateCommon },

    /// The maker received an order from the taker to open the CFD but doesn't have a response yet.
    ///
    /// This state applies to the maker only.
    IncomingOrderRequest {
        common: CfdStateCommon,
        taker_id: TakerId,
    },

    /// The maker has accepted the CFD take request, but the contract is not set up on chain yet.
    ///
    /// This state applies to taker and maker.
    Accepted { common: CfdStateCommon },

    /// The maker rejected the CFD order.
    ///
    /// This state applies to taker and maker.
    /// This is a final state.
    Rejected { common: CfdStateCommon },

    /// State used during contract setup.
    ///
    /// This state applies to taker and maker.
    /// All contract setup messages between taker and maker are expected to be sent in on scope.
    ContractSetup { common: CfdStateCommon },

    PendingOpen {
        common: CfdStateCommon,
        dlc: Dlc,
        attestation: Option<Attestation>,
    },

    /// The CFD contract is set up on chain.
    ///
    /// This state applies to taker and maker.
    Open {
        common: CfdStateCommon,
        dlc: Dlc,
        attestation: Option<Attestation>,
    },

    /// The commit transaction was published but it not final yet
    ///
    /// This state applies to taker and maker.
    /// This state is needed, because otherwise the user does not get any feedback.
    PendingCommit {
        common: CfdStateCommon,
        dlc: Dlc,
        attestation: Option<Attestation>,
    },

    // TODO: At the moment we are appending to this state. The way this is handled internally is
    //  by inserting the same state with more information in the database. We could consider
    //  changing this to insert different states or update the stae instead of inserting again.
    /// The CFD contract's commit transaction reached finality on chain
    ///
    /// This means that the commit transaction was detected on chain and reached finality
    /// confirmations and the contract will be forced to close.
    OpenCommitted {
        common: CfdStateCommon,
        dlc: Dlc,
        cet_status: CetStatus,
    },

    /// The CET was published on chain but is not final yet
    ///
    /// This state applies to taker and maker.
    /// This state is needed, because otherwise the user does not get any feedback.
    PendingCet {
        common: CfdStateCommon,
        dlc: Dlc,
        cet_status: CetStatus,
    },

    /// The position was closed collaboratively or non-collaboratively
    ///
    /// This state applies to taker and maker.
    /// This is a final state.
    /// This is the final state for all happy-path scenarios where we had an open position and then
    /// "settled" it. Settlement can be collaboratively or non-collaboratively (by publishing
    /// commit + cet).
    Closed { common: CfdStateCommon },

    // TODO: Can be extended with CetStatus
    /// The CFD contract's refund transaction was published but it not final yet
    MustRefund { common: CfdStateCommon, dlc: Dlc },

    /// The Cfd was refunded and the refund transaction reached finality
    ///
    /// This state applies to taker and maker.
    /// This is a final state.
    Refunded { common: CfdStateCommon },

    /// The Cfd was in a state that could not be continued after the application got interrupted
    ///
    /// This state applies to taker and maker.
    /// This is a final state.
    /// It is safe to remove Cfds in this state from the database.
    SetupFailed {
        common: CfdStateCommon,
        info: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attestation {
    pub id: OracleEventId,
    pub price: u64,
    pub scalars: Vec<SecretKey>,
}

impl From<oracle::Attestation> for Attestation {
    fn from(attestation: oracle::Attestation) -> Self {
        Attestation {
            id: attestation.id,
            price: attestation.price,
            scalars: attestation.scalars,
        }
    }
}

impl From<Attestation> for oracle::Attestation {
    fn from(attestation: Attestation) -> oracle::Attestation {
        oracle::Attestation {
            id: attestation.id,
            price: attestation.price,
            scalars: attestation.scalars,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum CetStatus {
    Unprepared,
    TimelockExpired,
    OracleSigned(Attestation),
    Ready(Attestation),
}

impl fmt::Display for CetStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CetStatus::Unprepared => write!(f, "Unprepared"),
            CetStatus::TimelockExpired => write!(f, "TimelockExpired"),
            CetStatus::OracleSigned(_) => write!(f, "OracleSigned"),
            CetStatus::Ready(_) => write!(f, "Ready"),
        }
    }
}

impl CfdState {
    fn get_common(&self) -> CfdStateCommon {
        let common = match self {
            CfdState::OutgoingOrderRequest { common } => common,
            CfdState::IncomingOrderRequest { common, .. } => common,
            CfdState::Accepted { common } => common,
            CfdState::Rejected { common } => common,
            CfdState::ContractSetup { common } => common,
            CfdState::PendingOpen { common, .. } => common,
            CfdState::Open { common, .. } => common,
            CfdState::OpenCommitted { common, .. } => common,
            CfdState::MustRefund { common, .. } => common,
            CfdState::Refunded { common, .. } => common,
            CfdState::SetupFailed { common, .. } => common,
            CfdState::PendingCommit { common, .. } => common,
            CfdState::PendingCet { common, .. } => common,
            CfdState::Closed { common, .. } => common,
        };

        *common
    }

    pub fn get_transition_timestamp(&self) -> SystemTime {
        self.get_common().transition_timestamp
    }
}

impl fmt::Display for CfdState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CfdState::OutgoingOrderRequest { .. } => {
                write!(f, "Request sent")
            }
            CfdState::IncomingOrderRequest { .. } => {
                write!(f, "Requested")
            }
            CfdState::Accepted { .. } => {
                write!(f, "Accepted")
            }
            CfdState::Rejected { .. } => {
                write!(f, "Rejected")
            }
            CfdState::ContractSetup { .. } => {
                write!(f, "Contract Setup")
            }
            CfdState::PendingOpen { .. } => {
                write!(f, "Pending Open")
            }
            CfdState::Open { .. } => {
                write!(f, "Open")
            }
            CfdState::PendingCommit { .. } => {
                write!(f, "Pending Commit")
            }
            CfdState::OpenCommitted { .. } => {
                write!(f, "Open Committed")
            }
            CfdState::MustRefund { .. } => {
                write!(f, "Must Refund")
            }
            CfdState::Refunded { .. } => {
                write!(f, "Refunded")
            }
            CfdState::SetupFailed { .. } => {
                write!(f, "Setup Failed")
            }
            CfdState::PendingCet { .. } => {
                write!(f, "Pending CET")
            }
            CfdState::Closed { .. } => {
                write!(f, "Closed")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum UpdateCfdProposal {
    Settlement {
        proposal: SettlementProposal,
        direction: SettlementKind,
    },
    RollOverProposal {
        proposal: RollOverProposal,
        direction: SettlementKind,
    },
}

/// Proposed collaborative settlement
#[derive(Debug, Clone)]
pub struct SettlementProposal {
    pub order_id: OrderId,
    pub timestamp: SystemTime,
    pub taker: Amount,
    pub maker: Amount,
}

/// Proposed collaborative settlement
#[derive(Debug, Clone)]
pub struct RollOverProposal {
    pub order_id: OrderId,
    pub timestamp: SystemTime,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Variants (for now) used by different binaries.
pub enum SettlementKind {
    Incoming,
    Outgoing,
}

pub type UpdateCfdProposals = HashMap<OrderId, UpdateCfdProposal>;

/// Represents a cfd (including state)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cfd {
    pub order: Order,
    pub quantity_usd: Usd,
    pub state: CfdState,
    /* TODO: Leverage is currently derived from the Order, but the actual leverage should be
     * stored in the Cfd once there is multiple choices of leverage */
}

impl Cfd {
    pub fn new(order: Order, quantity: Usd, state: CfdState) -> Self {
        Cfd {
            order,
            quantity_usd: quantity,
            state,
        }
    }

    pub fn margin(&self) -> Result<Amount> {
        let margin = match self.position() {
            Position::Buy => {
                calculate_buy_margin(self.order.price, self.quantity_usd, self.order.leverage)?
            }
            Position::Sell => calculate_sell_margin(self.order.price, self.quantity_usd)?,
        };

        Ok(margin)
    }

    pub fn counterparty_margin(&self) -> Result<Amount> {
        let margin = match self.position() {
            Position::Buy => calculate_sell_margin(self.order.price, self.quantity_usd)?,
            Position::Sell => {
                calculate_buy_margin(self.order.price, self.quantity_usd, self.order.leverage)?
            }
        };

        Ok(margin)
    }

    pub fn profit(&self, current_price: Usd) -> Result<(SignedAmount, Percent)> {
        let (p_n_l, p_n_l_percent) = calculate_profit(
            self.order.price,
            current_price,
            self.quantity_usd,
            self.margin()?,
            self.position(),
        )?;

        Ok((p_n_l, p_n_l_percent))
    }

    #[allow(dead_code)] // Not used by all binaries.
    pub fn calculate_settlement(&self, _current_price: Usd) -> Result<SettlementProposal> {
        // TODO: Calculate values for taker and maker
        // For the time being, assume that everybody loses :)
        let settlement = SettlementProposal {
            order_id: self.order.id,
            timestamp: SystemTime::now(),
            taker: Amount::ZERO,
            maker: Amount::ZERO,
        };

        Ok(settlement)
    }

    pub fn position(&self) -> Position {
        match self.order.origin {
            Origin::Ours => self.order.position.clone(),

            // If the order is not our own we take the counter-position in the CFD
            Origin::Theirs => match self.order.position {
                Position::Buy => Position::Sell,
                Position::Sell => Position::Buy,
            },
        }
    }

    #[allow(dead_code)]
    pub fn refund_timelock_in_blocks(&self) -> u32 {
        (self.order.term * Cfd::REFUND_THRESHOLD).as_blocks().ceil() as u32
    }

    /// A factor to be added to the CFD order term for calculating the refund timelock.
    ///
    /// The refund timelock is important in case the oracle disappears or never publishes a
    /// signature. Ideally, both users collaboratively settle in the refund scenario. This
    /// factor is important if the users do not settle collaboratively.
    /// `1.5` times the term as defined in CFD order should be safe in the extreme case where a user
    /// publishes the commit transaction right after the contract was initialized. In this case, the
    /// oracle still has `1.0 * cfdorder.term` time to attest and no one can publish the refund
    /// transaction.
    /// The downside is that if the oracle disappears: the users would only notice at the end
    /// of the cfd term. In this case the users has to wait for another `1.5` times of the
    /// term to get his funds back.
    #[allow(dead_code)]
    const REFUND_THRESHOLD: f32 = 1.5;

    #[allow(dead_code)]
    pub const CET_TIMELOCK: u32 = 12;

    pub fn handle(&mut self, event: CfdStateChangeEvent) -> Result<Option<CfdState>> {
        use CfdState::*;

        // TODO: Display impl
        tracing::info!("Cfd state change event {:?}", event);

        let order_id = self.order.id;

        // early exit if already final
        if let SetupFailed { .. } | Closed { .. } | Refunded { .. } = self.state.clone() {
            tracing::trace!(
                "Ignoring event {:?} because cfd already in state {}",
                event,
                self.state
            );
            return Ok(None);
        }

        let new_state = match event {
            CfdStateChangeEvent::Monitor(event) => match event {
                monitor::Event::LockFinality(_) => {
                    if let PendingOpen { dlc, .. } = self.state.clone() {
                        CfdState::Open {
                            common: CfdStateCommon {
                                transition_timestamp: SystemTime::now(),
                            },
                            dlc,
                            attestation: None,
                        }
                    } else if let Open {
                        dlc, attestation, ..
                    } = self.state.clone()
                    {
                        CfdState::Open {
                            common: CfdStateCommon {
                                transition_timestamp: SystemTime::now(),
                            },
                            dlc,
                            attestation,
                        }
                    } else {
                        bail!(
                            "Cannot transition to Open because of unexpected state {}",
                            self.state
                        )
                    }
                }
                monitor::Event::CommitFinality(_) => {
                    let (dlc, attestation) = if let PendingCommit {
                        dlc, attestation, ..
                    } = self.state.clone()
                    {
                        (dlc, attestation)
                    } else if let PendingOpen {
                        dlc, attestation, ..
                    }
                    | Open {
                        dlc, attestation, ..
                    } = self.state.clone()
                    {
                        tracing::debug!(%order_id, "Was in unexpected state {}, jumping ahead to OpenCommitted", self.state);
                        (dlc, attestation)
                    } else {
                        bail!(
                            "Cannot transition to OpenCommitted because of unexpected state {}",
                            self.state
                        )
                    };

                    OpenCommitted {
                        common: CfdStateCommon {
                            transition_timestamp: SystemTime::now(),
                        },
                        dlc,
                        cet_status: if let Some(attestation) = attestation {
                            CetStatus::OracleSigned(attestation)
                        } else {
                            CetStatus::Unprepared
                        },
                    }
                }
                monitor::Event::CetTimelockExpired(_) => match self.state.clone() {
                    CfdState::OpenCommitted {
                        dlc,
                        cet_status: CetStatus::Unprepared,
                        ..
                    } => CfdState::OpenCommitted {
                        common: CfdStateCommon {
                            transition_timestamp: SystemTime::now(),
                        },
                        dlc,
                        cet_status: CetStatus::TimelockExpired,
                    },
                    CfdState::OpenCommitted {
                        dlc,
                        cet_status: CetStatus::OracleSigned(attestation),
                        ..
                    } => CfdState::OpenCommitted {
                        common: CfdStateCommon {
                            transition_timestamp: SystemTime::now(),
                        },
                        dlc,
                        cet_status: CetStatus::Ready(attestation),
                    },
                    PendingOpen {
                        dlc, attestation, ..
                    }
                    | Open {
                        dlc, attestation, ..
                    }
                    | PendingCommit {
                        dlc, attestation, ..
                    } => {
                        tracing::debug!(%order_id, "Was in unexpected state {}, jumping ahead to OpenCommitted", self.state);
                        CfdState::OpenCommitted {
                            common: CfdStateCommon {
                                transition_timestamp: SystemTime::now(),
                            },
                            dlc,
                            cet_status: match attestation {
                                None => CetStatus::TimelockExpired,
                                Some(attestation) => CetStatus::Ready(attestation),
                            },
                        }
                    }
                    _ => bail!(
                        "Cannot transition to OpenCommitted because of unexpected state {}",
                        self.state
                    ),
                },
                monitor::Event::RefundTimelockExpired(_) => {
                    let dlc = if let OpenCommitted { dlc, .. } = self.state.clone() {
                        dlc
                    } else if let Open { dlc, .. } | PendingOpen { dlc, .. } = self.state.clone() {
                        tracing::debug!(%order_id, "Was in unexpected state {}, jumping ahead to MustRefund", self.state);
                        dlc
                    } else {
                        bail!(
                            "Cannot transition to MustRefund because of unexpected state {}",
                            self.state
                        )
                    };

                    MustRefund {
                        common: CfdStateCommon {
                            transition_timestamp: SystemTime::now(),
                        },
                        dlc,
                    }
                }
                monitor::Event::RefundFinality(_) => {
                    if let MustRefund { .. } = self.state.clone() {
                    } else {
                        tracing::debug!(
                            "Was in unexpected state {}, jumping ahead to Refunded",
                            self.state
                        );
                    }

                    Refunded {
                        common: CfdStateCommon {
                            transition_timestamp: SystemTime::now(),
                        },
                    }
                }
                monitor::Event::CetFinality(_) => Closed {
                    common: CfdStateCommon {
                        transition_timestamp: SystemTime::now(),
                    },
                },
                monitor::Event::RevokedTransactionFound(_) => {
                    todo!("Punish bad counterparty")
                }
            },
            CfdStateChangeEvent::CommitTxSent => {
                let (dlc, attestation) = if let PendingOpen {
                    dlc, attestation, ..
                }
                | Open {
                    dlc, attestation, ..
                } = self.state.clone()
                {
                    (dlc, attestation)
                } else {
                    bail!(
                        "Cannot transition to PendingCommit because of unexpected state {}",
                        self.state
                    )
                };

                PendingCommit {
                    common: CfdStateCommon {
                        transition_timestamp: SystemTime::now(),
                    },
                    dlc,
                    attestation,
                }
            }
            CfdStateChangeEvent::OracleAttestation(attestation) => match self.state.clone() {
                CfdState::PendingOpen { dlc, .. } | CfdState::Open { dlc, .. } => CfdState::Open {
                    common: CfdStateCommon {
                        transition_timestamp: SystemTime::now(),
                    },
                    dlc,
                    attestation: Some(attestation),
                },
                CfdState::PendingCommit { dlc, .. } => CfdState::PendingCommit {
                    common: CfdStateCommon {
                        transition_timestamp: SystemTime::now(),
                    },
                    dlc,
                    attestation: Some(attestation),
                },
                CfdState::OpenCommitted {
                    dlc,
                    cet_status: CetStatus::Unprepared,
                    ..
                } => CfdState::OpenCommitted {
                    common: CfdStateCommon {
                        transition_timestamp: SystemTime::now(),
                    },
                    dlc,
                    cet_status: CetStatus::OracleSigned(attestation),
                },
                CfdState::OpenCommitted {
                    dlc,
                    cet_status: CetStatus::TimelockExpired,
                    ..
                } => CfdState::OpenCommitted {
                    common: CfdStateCommon {
                        transition_timestamp: SystemTime::now(),
                    },
                    dlc,
                    cet_status: CetStatus::Ready(attestation),
                },
                _ => bail!(
                    "Cannot transition to OpenCommitted because of unexpected state {}",
                    self.state
                ),
            },
            CfdStateChangeEvent::CetSent => match self.state.clone() {
                CfdState::OpenCommitted {
                    common,
                    dlc,
                    cet_status,
                } => CfdState::PendingCet {
                    common,
                    dlc,
                    cet_status,
                },
                _ => bail!(
                    "Cannot transition to PendingCet because of unexpected state {}",
                    self.state
                ),
            },
        };

        self.state = new_state.clone();

        Ok(Some(new_state))
    }

    pub fn refund_tx(&self) -> Result<Transaction> {
        let dlc = if let CfdState::MustRefund { dlc, .. } = self.state.clone() {
            dlc
        } else {
            bail!("Refund transaction can only be constructed when in state MustRefund, but we are currently in {}", self.state.clone())
        };

        let sig_hash = spending_tx_sighash(
            &dlc.refund.0,
            &dlc.commit.2,
            Amount::from_sat(dlc.commit.0.output[0].value),
        );
        let our_sig = SECP256K1.sign(&sig_hash, &dlc.identity);
        let our_pubkey = PublicKey::new(bdk::bitcoin::secp256k1::PublicKey::from_secret_key(
            SECP256K1,
            &dlc.identity,
        ));
        let counterparty_sig = dlc.refund.1;
        let counterparty_pubkey = dlc.identity_counterparty;
        let signed_refund_tx = finalize_spend_transaction(
            dlc.refund.0,
            &dlc.commit.2,
            (our_pubkey, our_sig),
            (counterparty_pubkey, counterparty_sig),
        )?;

        Ok(signed_refund_tx)
    }

    pub fn commit_tx(&self) -> Result<Transaction> {
        let dlc = if let CfdState::Open { dlc, .. }
        | CfdState::PendingOpen { dlc, .. }
        | CfdState::PendingCommit { dlc, .. } = self.state.clone()
        {
            dlc
        } else {
            bail!(
                "Cannot publish commit transaction in state {}",
                self.state.clone()
            )
        };

        let sig_hash = spending_tx_sighash(
            &dlc.commit.0,
            &dlc.lock.1,
            Amount::from_sat(dlc.lock.0.output[0].value),
        );
        let our_sig = SECP256K1.sign(&sig_hash, &dlc.identity);
        let our_pubkey = PublicKey::new(bdk::bitcoin::secp256k1::PublicKey::from_secret_key(
            SECP256K1,
            &dlc.identity,
        ));

        let counterparty_sig = dlc.commit.1.decrypt(&dlc.publish)?;
        let counterparty_pubkey = dlc.identity_counterparty;

        let signed_commit_tx = finalize_spend_transaction(
            dlc.commit.0,
            &dlc.lock.1,
            (our_pubkey, our_sig),
            (counterparty_pubkey, counterparty_sig),
        )?;

        Ok(signed_commit_tx)
    }

    pub fn cet(&self) -> Result<Result<Transaction, NotReadyYet>> {
        let (dlc, attestation) = match self.state.clone() {
            CfdState::OpenCommitted {
                dlc,
                cet_status: CetStatus::Ready(attestation),
                ..
            }
            | CfdState::PendingCet {
                dlc,
                cet_status: CetStatus::Ready(attestation),
                ..
            } => (dlc, attestation),
            CfdState::OpenCommitted { cet_status, .. } => {
                return Ok(Err(NotReadyYet { cet_status }));
            }
            CfdState::Open { .. } | CfdState::PendingCommit { .. } => {
                return Ok(Err(NotReadyYet {
                    cet_status: CetStatus::Unprepared,
                }));
            }
            _ => bail!("Cannot publish CET in state {}", self.state.clone()),
        };

        let cets = dlc
            .cets
            .get(&attestation.id)
            .context("Unable to find oracle event id within the cets of the dlc")?;

        let Cet {
            tx: cet,
            adaptor_sig: encsig,
            n_bits,
            ..
        } = cets
            .iter()
            .find(|Cet { range, .. }| range.contains(&attestation.price))
            .context("Price out of range of cets")?;

        let oracle_attestations = attestation.scalars;

        let mut decryption_sk = oracle_attestations[0];
        for oracle_attestation in oracle_attestations[1..*n_bits].iter() {
            decryption_sk.add_assign(oracle_attestation.as_ref())?;
        }

        let sig_hash = spending_tx_sighash(
            cet,
            &dlc.commit.2,
            Amount::from_sat(dlc.commit.0.output[0].value),
        );
        let our_sig = SECP256K1.sign(&sig_hash, &dlc.identity);
        let our_pubkey = PublicKey::new(bdk::bitcoin::secp256k1::PublicKey::from_secret_key(
            SECP256K1,
            &dlc.identity,
        ));

        let counterparty_sig = encsig.decrypt(&decryption_sk)?;
        let counterparty_pubkey = dlc.identity_counterparty;

        let signed_cet = finalize_spend_transaction(
            cet.clone(),
            &dlc.commit.2,
            (our_pubkey, our_sig),
            (counterparty_pubkey, counterparty_sig),
        )?;

        Ok(Ok(signed_cet))
    }

    pub fn pending_open_dlc(&self) -> Option<Dlc> {
        if let CfdState::PendingOpen { dlc, .. } = self.state.clone() {
            Some(dlc)
        } else {
            None
        }
    }

    pub fn open_dlc(&self) -> Option<Dlc> {
        if let CfdState::Open { dlc, .. } = self.state.clone() {
            Some(dlc)
        } else {
            None
        }
    }

    pub fn is_must_refund(&self) -> bool {
        matches!(self.state.clone(), CfdState::MustRefund { .. })
    }

    pub fn is_pending_commit(&self) -> bool {
        matches!(self.state.clone(), CfdState::PendingCommit { .. })
    }

    pub fn is_pending_cet(&self) -> bool {
        matches!(self.state.clone(), CfdState::PendingCet { .. })
    }

    pub fn is_cleanup(&self) -> bool {
        matches!(
            self.state.clone(),
            CfdState::OutgoingOrderRequest { .. }
                | CfdState::IncomingOrderRequest { .. }
                | CfdState::Accepted { .. }
                | CfdState::ContractSetup { .. }
        )
    }

    pub fn role(&self) -> Role {
        self.order.origin.into()
    }
}

#[derive(thiserror::Error, Debug, Clone)]
#[error("The cfd is not ready for CET publication yet: {cet_status}")]
pub struct NotReadyYet {
    cet_status: CetStatus,
}

#[derive(Debug, Clone)]
pub enum CfdStateChangeEvent {
    // TODO: group other events by actors into enums and add them here so we can bundle all
    // transitions into cfd.transition_to(...)
    Monitor(monitor::Event),
    CommitTxSent,
    OracleAttestation(Attestation),
    CetSent,
}

/// Returns the Profit/Loss (P/L) as Bitcoin. Losses are capped by the provided margin
fn calculate_profit(
    initial_price: Usd,
    current_price: Usd,
    quantity: Usd,
    margin: Amount,
    position: Position,
) -> Result<(SignedAmount, Percent)> {
    let margin_as_sat =
        Decimal::from_u64(margin.as_sat()).context("Expect to be a valid decimal")?;

    let initial_price_inverse = dec!(1)
        .checked_div(initial_price.0)
        .context("Calculating inverse of initial_price resulted in overflow")?;
    let current_price_inverse = dec!(1)
        .checked_div(current_price.0)
        .context("Calculating inverse of current_price resulted in overflow")?;

    // calculate profit/loss (P and L) in BTC
    let profit_btc = match position {
        Position::Buy => {
            // for long: profit_btc = quantity_usd * ((1/initial_price)-(1/current_price))
            quantity
                .0
                .checked_mul(
                    initial_price_inverse
                        .checked_sub(current_price_inverse)
                        .context("Subtracting current_price_inverse from initial_price_inverse resulted in an overflow")?,
                )
        }
        Position::Sell => {
            // for short: profit_btc = quantity_usd * ((1/current_price)-(1/initial_price))
            quantity
                .0
                .checked_mul(
                    current_price_inverse
                        .checked_sub(initial_price_inverse)
                        .context("Subtracting initial_price_inverse from current_price_inverse resulted in an overflow")?,
                )
        }
    }
        .context("Calculating profit/loss resulted in an overflow")?;

    let sat_adjust = Decimal::from(Amount::ONE_BTC.as_sat());
    let profit_btc_as_sat = profit_btc
        .checked_mul(sat_adjust)
        .context("Could not adjust profit to satoshi")?;

    // loss cannot be more than provided margin
    let margin_plus_profit_btc = margin_as_sat
        .checked_add(profit_btc_as_sat)
        .context("Adding up margin and profit_btc resulted in an overflow")?;

    let in_percent = if profit_btc_as_sat.is_zero() {
        Decimal::ZERO
    } else {
        profit_btc_as_sat
            .checked_div(margin_as_sat)
            .context("Profit divided by margin resulted in overflow")?
    };

    if margin_plus_profit_btc < Decimal::ZERO {
        return Ok((
            SignedAmount::from_sat(
                margin_as_sat
                    .neg()
                    .to_i64()
                    .context("Could not convert margin to i64")?,
            ),
            dec!(-100).into(),
        ));
    }

    Ok((
        SignedAmount::from_sat(
            profit_btc_as_sat
                .to_i64()
                .context("Could not convert profit to i64")?,
        ),
        in_percent
            .checked_mul(dec!(100.0))
            .context("Converting to percent resulted in an overflow")?
            .into(),
    ))
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

/// Calculates the buyer's margin in BTC
///
/// The margin is the initial margin and represents the collateral the buyer has to come up with to
/// satisfy the contract. Here we calculate the initial buy margin as: quantity / (initial_price *
/// leverage)
pub fn calculate_buy_margin(price: Usd, quantity: Usd, leverage: Leverage) -> Result<Amount> {
    let leverage = Decimal::from(leverage.0).into();

    let margin = quantity.checked_div(price.checked_mul(leverage)?)?;

    let sat_adjust = Decimal::from(Amount::ONE_BTC.as_sat()).into();
    let margin = margin.checked_mul(sat_adjust)?;
    let margin = Amount::from_sat(margin.try_into_u64()?);

    Ok(margin)
}

/// Calculates the seller's margin in BTC
///
/// The seller margin is represented as the quantity of the contract given the initial price.
/// The seller can currently not leverage the position but always has to cover the complete
/// quantity.
fn calculate_sell_margin(price: Usd, quantity: Usd) -> Result<Amount> {
    let margin = quantity.checked_div(price)?;

    let sat_adjust = Decimal::from(Amount::ONE_BTC.as_sat()).into();
    let margin = margin.checked_mul(sat_adjust)?;
    let margin = Amount::from_sat(margin.try_into_u64()?);

    Ok(margin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn given_default_values_then_expected_liquidation_price() {
        let leverage = Leverage(5);
        let price = Usd(dec!(49000));
        let maintenance_margin_rate = dec!(0.005);

        let liquidation_price =
            calculate_liquidation_price(&leverage, &price, &maintenance_margin_rate).unwrap();

        assert_eq!(liquidation_price, Usd(dec!(41004.184100418410041841004184)));
    }

    #[test]
    fn given_leverage_of_one_and_equal_price_and_quantity_then_buy_margin_is_one_btc() {
        let price = Usd(dec!(40000));
        let quantity = Usd(dec![40000]);
        let leverage = Leverage(1);

        let buy_margin = calculate_buy_margin(price, quantity, leverage).unwrap();

        assert_eq!(buy_margin, Amount::ONE_BTC);
    }

    #[test]
    fn given_leverage_of_one_and_leverage_of_ten_then_buy_margin_is_lower_factor_ten() {
        let price = Usd(dec!(40000));
        let quantity = Usd(dec![40000]);
        let leverage = Leverage(10);

        let buy_margin = calculate_buy_margin(price, quantity, leverage).unwrap();

        assert_eq!(buy_margin, Amount::from_btc(0.1).unwrap());
    }

    #[test]
    fn given_quantity_equals_price_then_sell_margin_is_one_btc() {
        let price = Usd(dec!(40000));
        let quantity = Usd(dec![40000]);

        let sell_margin = calculate_sell_margin(price, quantity).unwrap();

        assert_eq!(sell_margin, Amount::ONE_BTC);
    }

    #[test]
    fn given_quantity_half_of_price_then_sell_margin_is_half_btc() {
        let price = Usd(dec!(40000));
        let quantity = Usd(dec![20000]);

        let sell_margin = calculate_sell_margin(price, quantity).unwrap();

        assert_eq!(sell_margin, Amount::from_btc(0.5).unwrap());
    }

    #[test]
    fn given_quantity_double_of_price_then_sell_margin_is_two_btc() {
        let price = Usd(dec!(40000));
        let quantity = Usd(dec![80000]);

        let sell_margin = calculate_sell_margin(price, quantity).unwrap();

        assert_eq!(sell_margin, Amount::from_btc(2.0).unwrap());
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
        assert_profit_loss_values(
            Usd::from(dec!(10_000)),
            Usd::from(dec!(10_000)),
            Usd::from(dec!(10_000)),
            Amount::ONE_BTC,
            Position::Buy,
            SignedAmount::ZERO,
            Decimal::ZERO.into(),
            "No price increase means no profit",
        );

        assert_profit_loss_values(
            Usd::from(dec!(10_000)),
            Usd::from(dec!(20_000)),
            Usd::from(dec!(10_000)),
            Amount::ONE_BTC,
            Position::Buy,
            SignedAmount::from_sat(50_000_000),
            dec!(50).into(),
            "A 100% price increase should be 50% profit",
        );

        assert_profit_loss_values(
            Usd::from(dec!(10_000)),
            Usd::from(dec!(5_000)),
            Usd::from(dec!(10_000)),
            Amount::ONE_BTC,
            Position::Buy,
            SignedAmount::from_sat(-100_000_000),
            dec!(-100).into(),
            "A 50% drop should result in 100% loss",
        );

        assert_profit_loss_values(
            Usd::from(dec!(10_000)),
            Usd::from(dec!(2_500)),
            Usd::from(dec!(10_000)),
            Amount::ONE_BTC,
            Position::Buy,
            SignedAmount::from_sat(-100_000_000),
            dec!(-100).into(),
            "A loss should be capped by 100%",
        );

        assert_profit_loss_values(
            Usd::from(dec!(50_400)),
            Usd::from(dec!(60_000)),
            Usd::from(dec!(10_000)),
            Amount::from_btc(0.01984).unwrap(),
            Position::Buy,
            SignedAmount::from_sat(3_174_603),
            dec!(160.01024065540194572452620968).into(),
            "buy position should make a profit when price goes up",
        );

        assert_profit_loss_values(
            Usd::from(dec!(10_000)),
            Usd::from(dec!(16_000)),
            Usd::from(dec!(10_000)),
            Amount::ONE_BTC,
            Position::Sell,
            SignedAmount::from_sat(-37_500_000),
            dec!(-37.5).into(),
            "sell position should make a loss when price goes up",
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn assert_profit_loss_values(
        initial_price: Usd,
        current_price: Usd,
        quantity: Usd,
        margin: Amount,
        position: Position,
        should_profit: SignedAmount,
        should_profit_in_percent: Percent,
        msg: &str,
    ) {
        let (profit, in_percent) =
            calculate_profit(initial_price, current_price, quantity, margin, position).unwrap();

        assert_eq!(profit, should_profit, "{}", msg);
        assert_eq!(in_percent, should_profit_in_percent, "{}", msg);
    }

    #[test]
    fn test_profit_calculation_loss_plus_profit_should_be_zero() {
        let initial_price = Usd::from(dec!(10_000));
        let closing_price = Usd::from(dec!(16_000));
        let quantity = Usd::from(dec!(10_000));
        let margin = Amount::ONE_BTC;
        let (profit, profit_in_percent) = calculate_profit(
            initial_price,
            closing_price,
            quantity,
            margin,
            Position::Buy,
        )
        .unwrap();
        let (loss, loss_in_percent) = calculate_profit(
            initial_price,
            closing_price,
            quantity,
            margin,
            Position::Sell,
        )
        .unwrap();

        assert_eq!(profit.checked_add(loss).unwrap(), SignedAmount::ZERO);
        assert_eq!(
            profit_in_percent.0.checked_add(loss_in_percent.0).unwrap(),
            Decimal::ZERO
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cet {
    pub tx: Transaction,
    pub adaptor_sig: EcdsaAdaptorSignature,

    // TODO: Range + number of digits (usize) could be represented as Digits similar to what we do
    // in the protocol lib
    pub range: RangeInclusive<u64>,
    pub n_bits: usize,
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
    pub cets: HashMap<OracleEventId, Vec<Cet>>,
    pub refund: (Transaction, Signature),

    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_sat")]
    pub maker_lock_amount: Amount,
    #[serde(with = "::bdk::bitcoin::util::amount::serde::as_sat")]
    pub taker_lock_amount: Amount,

    pub revoked_commit: Vec<RevokedCommit>,
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
