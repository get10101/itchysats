use std::time::Duration;

use crate::bitcoin::secp256k1::ecdsa::Signature;
use crate::bitcoin::Transaction;
use anyhow::anyhow;
use anyhow::Result;
use model::hex_transaction;
use model::OrderId;
use model::Price;
use serde::Deserialize;
use serde::Serialize;

pub const SETTLEMENT_MSG_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Serialize, Deserialize)]
pub enum DialerMessage {
    Propose(Propose),
    DialerSignature(DialerSignature),
}

impl DialerMessage {
    pub fn into_propose(self) -> Result<Propose> {
        match self {
            DialerMessage::Propose(propose) => Ok(propose),
            DialerMessage::DialerSignature(_) => {
                Err(anyhow!("Expected Propose but got DialerSignature"))
            }
        }
    }

    pub fn into_dialer_signature(self) -> Result<DialerSignature> {
        match self {
            DialerMessage::DialerSignature(dialer_signature) => Ok(dialer_signature),
            DialerMessage::Propose(_) => Err(anyhow!("Expected DialerSignature but got Propose")),
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum ListenerMessage {
    Decision(Decision),
    ListenerSignature(ListenerSignature),
}

impl ListenerMessage {
    pub fn into_decision(self) -> Result<Decision> {
        match self {
            ListenerMessage::Decision(decision) => Ok(decision),
            ListenerMessage::ListenerSignature(_) => {
                Err(anyhow!("Expected Decision but got ListenerSignature"))
            }
        }
    }

    pub fn into_listener_signature(self) -> Result<ListenerSignature> {
        match self {
            ListenerMessage::ListenerSignature(listener_signature) => Ok(listener_signature),
            ListenerMessage::Decision(_) => {
                Err(anyhow!("Expected ListenerSignature but got Decision"))
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Propose {
    pub id: OrderId,
    pub price: Price,
    /// The transaction that is being proposed to close the protocol.
    ///
    /// Sending the full transaction allows the listening side to verify, how exactly the dialing
    /// side wants to perform collaborative settlement.
    #[serde(with = "hex_transaction")]
    pub unsigned_tx: Transaction,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum Decision {
    Accept,
    Reject,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct DialerSignature {
    pub dialer_signature: Signature,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct ListenerSignature {
    pub listener_signature: Signature,
}
