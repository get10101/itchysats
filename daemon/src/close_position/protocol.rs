use crate::bitcoin::secp256k1::Signature;
use crate::bitcoin::Transaction;
use crate::close_position::PROTOCOL;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::PeerId;
use model::hex_transaction;
use model::ClosePositionTransaction;
use model::CollaborativeSettlement;
use model::OrderId;
use model::Price;
use serde::Deserialize;
use serde::Serialize;
use xtra::Address;
use xtra_libp2p::Endpoint;
use xtra_libp2p::OpenSubstream;

pub async fn dialer(
    endpoint: Address<Endpoint>,
    order_id: OrderId,
    counterparty: PeerId,
    close_position_tx: ClosePositionTransaction,
) -> Result<CollaborativeSettlement, DialerFailed> {
    let substream = endpoint
        .send(OpenSubstream::single_protocol(counterparty, PROTOCOL))
        .await
        .context("Endpoint is disconnected")?
        .context("Failed to open substream")?;
    let mut framed = asynchronous_codec::Framed::new(
        substream,
        asynchronous_codec::JsonCodec::<DialerMessage, ListenerMessage>::new(),
    );

    let unsigned_tx = close_position_tx.unsigned_transaction();

    framed
        .send(DialerMessage::Msg0(Msg0 {
            id: order_id,
            price: close_position_tx.price(),
            unsigned_tx: unsigned_tx.clone(),
        }))
        .await
        .context("Failed to send Msg0")?;

    // TODO: We will need to apply a timeout to these. Perhaps we can put a timeout generally into
    // "reading from the substream"?
    if let Msg1::Reject = framed
        .next()
        .await
        .context("End of stream while receiving Msg1")?
        .context("Failed to decode Msg1")?
        .into_msg1()?
    {
        return Err(DialerFailed::Rejected);
    }

    framed
        .send(DialerMessage::Msg2(Msg2 {
            dialer_signature: close_position_tx.own_signature(),
        }))
        .await
        .context("Failed to send Msg2")?;

    let msg3 = match framed.next().await {
        Some(Ok(msg)) => msg.into_msg3()?,
        Some(Err(_)) | None => {
            return Err(DialerFailed::AfterSendingSignature { unsigned_tx });
        }
    };

    let close_position_tx =
        match close_position_tx.recv_counterparty_signature(msg3.listener_signature) {
            Ok(close_position_tx) => close_position_tx,
            Err(error) => {
                // TODO: What to do in case of an invalid signature?

                return Err(DialerFailed::AfterSendingSignature { unsigned_tx });
            }
        };

    let settlement = close_position_tx.finalize()?; // TODO: What to do if we fail here?

    Ok(settlement)
}

#[derive(Debug)]
pub enum DialerFailed {
    Rejected,
    AfterSendingSignature { unsigned_tx: Transaction },
    BeforeSendingSignature { source: anyhow::Error },
}

impl From<anyhow::Error> for DialerFailed {
    fn from(source: anyhow::Error) -> Self {
        Self::BeforeSendingSignature { source }
    }
}

#[derive(Serialize, Deserialize)]
pub enum DialerMessage {
    Msg0(Msg0),
    Msg2(Msg2),
}

impl DialerMessage {
    pub fn into_msg0(self) -> Result<Msg0> {
        match self {
            DialerMessage::Msg0(msg0) => Ok(msg0),
            DialerMessage::Msg2(_) => Err(anyhow!("Expected Msg0 but got Msg2")),
        }
    }

    pub fn into_msg2(self) -> Result<Msg2> {
        match self {
            DialerMessage::Msg2(msg2) => Ok(msg2),
            DialerMessage::Msg0(_) => Err(anyhow!("Expected Msg2 but got Msg0")),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub enum ListenerMessage {
    Msg1(Msg1),
    Msg3(Msg3),
}

impl ListenerMessage {
    pub fn into_msg1(self) -> Result<Msg1> {
        match self {
            ListenerMessage::Msg1(msg1) => Ok(msg1),
            ListenerMessage::Msg3(_) => Err(anyhow!("Expected Msg1 but got Msg3")),
        }
    }

    pub fn into_msg3(self) -> Result<Msg3> {
        match self {
            ListenerMessage::Msg3(msg3) => Ok(msg3),
            ListenerMessage::Msg1(_) => Err(anyhow!("Expected Msg3 but got Msg1")),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Msg0 {
    pub id: OrderId,
    pub price: Price,
    /// The transaction that is being proposed to close the protocol.
    ///
    /// Sending the full transaction allows the listening side to verify, how exactly the dialing
    /// side wants to close the position.
    #[serde(with = "hex_transaction")]
    pub unsigned_tx: Transaction,
}

#[derive(Serialize, Deserialize)]
pub enum Msg1 {
    Accept,
    Reject,
}

#[derive(Serialize, Deserialize)]
pub struct Msg2 {
    pub dialer_signature: Signature,
}

#[derive(Serialize, Deserialize)]
pub struct Msg3 {
    pub listener_signature: Signature,
}
