use crate::bitcoin::secp256k1::Signature;
use crate::bitcoin::Transaction;
use crate::close_position::PROTOCOL;
use crate::command;
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

    let unsigned_tx = close_position_tx.unsigned_transaction().clone();

    framed
        .send(DialerMessage::Propose(Propose {
            id: order_id,
            price: close_position_tx.price(),
            unsigned_tx: unsigned_tx.clone(),
        }))
        .await
        .context("Failed to send Msg0")?;

    // TODO: We will need to apply a timeout to these. Perhaps we can put a timeout generally into
    // "reading from the substream"?
    if let Decision::Reject = framed
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
            return Err(DialerFailed::AfterSendingSignature {
                unsigned_tx: unsigned_tx.clone(),
                error: anyhow!("failed to received msg3"),
            });
        }
    };

    let close_position_tx =
        match close_position_tx.recv_counterparty_signature(msg3.listener_signature) {
            Ok(close_position_tx) => close_position_tx,
            Err(error) => {
                return Err(DialerFailed::AfterSendingSignature {
                    unsigned_tx: unsigned_tx.clone(),
                    error,
                });
            }
        };

    let settlement =
        close_position_tx
            .finalize()
            .map_err(|e| DialerFailed::AfterSendingSignature {
                unsigned_tx: unsigned_tx.clone(),
                error: e,
            })?;

    Ok(settlement)
}

#[derive(Debug)]
pub enum DialerFailed {
    Rejected,
    AfterSendingSignature {
        unsigned_tx: Transaction,
        error: anyhow::Error,
    },
    BeforeSendingSignature {
        source: anyhow::Error,
    },
}

impl From<anyhow::Error> for DialerFailed {
    fn from(source: anyhow::Error) -> Self {
        Self::BeforeSendingSignature { source }
    }
}

#[derive(Serialize, Deserialize)]
pub enum DialerMessage {
    Propose(Propose),
    Msg2(Msg2),
}

impl DialerMessage {
    pub fn into_propose(self) -> Result<Propose> {
        match self {
            DialerMessage::Propose(msg0) => Ok(msg0),
            DialerMessage::Msg2(_) => Err(anyhow!("Expected Msg0 but got Msg2")),
        }
    }

    pub fn into_msg2(self) -> Result<Msg2> {
        match self {
            DialerMessage::Msg2(msg2) => Ok(msg2),
            DialerMessage::Propose(_) => Err(anyhow!("Expected Msg2 but got Msg0")),
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum ListenerMessage {
    Msg1(Decision),
    Msg3(Msg3),
}

impl ListenerMessage {
    pub fn into_msg1(self) -> Result<Decision> {
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
pub struct Propose {
    pub id: OrderId,
    pub price: Price,
    /// The transaction that is being proposed to close the protocol.
    ///
    /// Sending the full transaction allows the listening side to verify, how exactly the dialing
    /// side wants to close the position.
    #[serde(with = "hex_transaction")]
    pub unsigned_tx: Transaction,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum Decision {
    Accept,
    Reject,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Msg2 {
    pub dialer_signature: Signature,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Msg3 {
    pub listener_signature: Signature,
}

pub(crate) async fn emit_completed(
    order_id: OrderId,
    settlement: CollaborativeSettlement,
    executor: &command::Executor,
) {
    if let Err(e) = executor
        .execute(order_id, |cfd| {
            Ok(cfd.complete_collaborative_settlement(settlement))
        })
        .await
    {
        tracing::error!(%order_id, "Failed to execute `complete_collaborative_settlement` command: {e:#}");
    }

    tracing::info!(%order_id, "Rollover completed");
}

pub(crate) async fn emit_rejected(order_id: OrderId, executor: &command::Executor) {
    if let Err(e) = executor
        .execute(order_id, |cfd| {
            Ok(cfd.reject_collaborative_settlement(anyhow!("maker decision")))
        })
        .await
    {
        tracing::error!(%order_id, "Failed to execute `reject_collaborative_settlement` command: {e:#}")
    }

    tracing::info!(%order_id, "Rollover rejected");
}

pub(crate) async fn emit_failed(order_id: OrderId, e: anyhow::Error, executor: &command::Executor) {
    tracing::error!(%order_id, "Rollover failed: {e:#}");

    if let Err(e) = executor
        .execute(order_id, |cfd| Ok(cfd.fail_collaborative_settlement(e)))
        .await
    {
        tracing::error!(%order_id, "Failed to execute `fail_collaborative_settlement` command: {e:#}");
    }
}
