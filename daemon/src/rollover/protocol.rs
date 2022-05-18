use crate::wire::CompleteFee;
use crate::wire::RolloverMsg;
use anyhow::anyhow;
use anyhow::Result;
use model::olivia::BitMexPriceEventId;
use model::Dlc;
use model::FundingFee;
use model::FundingRate;
use model::OrderId;
use model::Timestamp;
use model::TxFeeRate;
use serde::Deserialize;
use serde::Serialize;

pub struct RolloverCompletedParams {
    pub dlc: Dlc,
    pub funding_fee: FundingFee,
}

#[derive(thiserror::Error, Debug)]
pub enum DialerError {
    #[error("Rollover got rejected")]
    Rejected,
    #[error("Rollover failed")]
    Failed { source: anyhow::Error },
}

impl From<anyhow::Error> for DialerError {
    fn from(source: anyhow::Error) -> Self {
        Self::Failed { source }
    }
}

#[derive(Serialize, Deserialize)]
pub enum DialerMessage {
    Propose(Propose),
    RolloverMsg(RolloverMsg),
}

impl DialerMessage {
    pub fn into_propose(self) -> Result<Propose> {
        match self {
            DialerMessage::Propose(propose) => Ok(propose),
            DialerMessage::RolloverMsg(_) => Err(anyhow!("Expected Propose but got RolloverMsg")),
        }
    }

    pub fn into_rollover_msg(self) -> Result<RolloverMsg> {
        match self {
            DialerMessage::RolloverMsg(rollover_msg) => Ok(rollover_msg),
            DialerMessage::Propose(_) => Err(anyhow!("Expected RolloverMsg but got Propose")),
        }
    }
}

#[derive(Copy, Clone, Serialize, Deserialize)]
pub enum Decision {
    Confirm(Confirm),
    Reject(Reject),
}

#[derive(Serialize, Deserialize)]
pub enum ListenerMessage {
    Decision(Decision),
    RolloverMsg(RolloverMsg),
}

impl ListenerMessage {
    pub fn into_decision(self) -> Result<Decision> {
        match self {
            ListenerMessage::Decision(decision) => Ok(decision),
            ListenerMessage::RolloverMsg(_) => {
                Err(anyhow!("Expected Decision but got RolloverMsg"))
            }
        }
    }

    pub fn into_rollover_msg(self) -> Result<RolloverMsg> {
        match self {
            ListenerMessage::RolloverMsg(rollover_msg) => Ok(rollover_msg),
            ListenerMessage::Decision(_) => Err(anyhow!("Expected RolloverMsg but got Decision")),
        }
    }
}

#[derive(Copy, Clone, Serialize, Deserialize)]
pub struct Propose {
    pub order_id: OrderId,
    pub timestamp: Timestamp,
}

#[derive(Copy, Clone, Serialize, Deserialize)]
pub struct Confirm {
    pub order_id: OrderId,
    pub oracle_event_id: BitMexPriceEventId,
    pub tx_fee_rate: TxFeeRate,
    pub funding_rate: FundingRate,
    pub complete_fee: CompleteFee,
}

#[derive(Copy, Clone, Serialize, Deserialize)]
pub struct Reject {
    pub order_id: OrderId,
}
