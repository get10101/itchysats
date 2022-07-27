use crate::wire::SetupMsg;
use anyhow::bail;
use anyhow::Result;
use model::Leverage;
use model::Offer;
use model::OrderId;
use model::Usd;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum TakerMessage {
    PlaceOrder {
        id: OrderId,
        offer: Offer,
        quantity: Usd,
        leverage: Leverage,
    },
    ContractSetupMsg(Box<SetupMsg>),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum MakerMessage {
    Decision(Decision),
    ContractSetupMsg(Box<SetupMsg>),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Decision {
    Accept,
    Reject,
}

impl TryFrom<MakerMessage> for SetupMsg {
    type Error = anyhow::Error;

    fn try_from(value: MakerMessage) -> Result<Self> {
        match value {
            MakerMessage::Decision(_) => bail!("Expected SetupMsg, got decision"),
            MakerMessage::ContractSetupMsg(msg) => Ok(*msg),
        }
    }
}

impl TryFrom<TakerMessage> for SetupMsg {
    type Error = anyhow::Error;

    fn try_from(value: TakerMessage) -> Result<Self> {
        match value {
            TakerMessage::PlaceOrder { .. } => bail!("Expected SetupMsg, got order placement"),
            TakerMessage::ContractSetupMsg(msg) => Ok(*msg),
        }
    }
}
