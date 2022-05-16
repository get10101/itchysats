use crate::wire::CompleteFee;
use crate::wire::RolloverMsg;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use futures::StreamExt;
use libp2p_core::PeerId;
use model::olivia::BitMexPriceEventId;
use model::FundingRate;
use model::OrderId;
use model::Price;
use model::Timestamp;
use model::TxFeeRate;
use serde::Deserialize;
use serde::Serialize;
use xtra::Address;
use xtra_libp2p::Endpoint;
use xtra_libp2p::OpenSubstream;

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

#[derive(Serialize, Deserialize)]
pub enum ListenerMessage {
    Confirm(Confirm),
    Reject(Reject),
    RolloverMsg(RolloverMsg),
}

impl ListenerMessage {
    pub fn into_confirm(self) -> Result<Confirm> {
        match self {
            ListenerMessage::Confirm(confirm) => Ok(confirm),
            ListenerMessage::Reject(_) => Err(anyhow!("Expected Confirm but got Reject")),
            ListenerMessage::RolloverMsg(_) => Err(anyhow!("Expected Confirm but got RolloverMsg")),
        }
    }

    pub fn into_reject(self) -> Result<Reject> {
        match self {
            ListenerMessage::Reject(reject) => Ok(reject),
            ListenerMessage::Confirm(_) => Err(anyhow!("Expected Reject but got Confirm")),
            ListenerMessage::RolloverMsg(_) => Err(anyhow!("Expected Confirm but got RolloverMsg")),
        }
    }

    pub fn into_rollover_msg(self) -> Result<RolloverMsg> {
        match self {
            ListenerMessage::RolloverMsg(rollover_msg) => Ok(rollover_msg),
            ListenerMessage::Reject(_) => Err(anyhow!("Expected Confirm but got Reject")),
            ListenerMessage::Confirm(_) => Err(anyhow!("Expected RolloverMsg but got Confirm")),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Propose {
    pub order_id: OrderId,
    pub timestamp: Timestamp,
}

#[derive(Serialize, Deserialize)]
pub struct Confirm {
    order_id: OrderId,
    oracle_event_id: BitMexPriceEventId,
    tx_fee_rate: TxFeeRate,
    funding_rate: FundingRate,
    complete_fee: CompleteFee,
}

#[derive(Serialize, Deserialize)]
pub struct Reject {
    order_id: OrderId,
}
