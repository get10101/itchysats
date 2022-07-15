use std::time::Duration;

use crate::bitcoin::secp256k1::ecdsa::Signature;
use crate::bitcoin::Transaction;
use crate::collab_settlement::PROTOCOL;
use crate::command;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::PeerId;
use model::hex_transaction;
use model::CollaborativeSettlement;
use model::OrderId;
use model::Price;
use model::SettlementTransaction;
use serde::Deserialize;
use serde::Serialize;
use tokio_extras::FutureExt;
use xtra::Address;
use xtra_libp2p::Endpoint;
use xtra_libp2p::OpenSubstream;

/// The duration that the taker waits until a decision (accept/reject) is expected from the maker
///
/// If the maker does not respond within `DECISION_TIMEOUT` seconds then the taker will fail the
/// collab settlement.
const DECISION_TIMEOUT: Duration = Duration::from_secs(30);

pub const SETTLEMENT_MSG_TIMEOUT: Duration = Duration::from_secs(120);

#[tracing::instrument(skip(endpoint, collab_settlement_tx))]
pub async fn dialer(
    endpoint: Address<Endpoint>,
    order_id: OrderId,
    counterparty: PeerId,
    collab_settlement_tx: SettlementTransaction,
) -> Result<CollaborativeSettlement, DialerFailed> {
    let substream = endpoint
        .send(OpenSubstream::single_protocol(counterparty, PROTOCOL))
        .await
        .context("Endpoint is disconnected")?
        .context("No connection to peer")?
        .await
        .context("Failed to open substream")?;
    let mut framed = asynchronous_codec::Framed::new(
        substream,
        asynchronous_codec::JsonCodec::<DialerMessage, ListenerMessage>::new(),
    );

    let unsigned_tx = collab_settlement_tx.unsigned_transaction().clone();

    framed
        .send(DialerMessage::Propose(Propose {
            id: order_id,
            price: collab_settlement_tx.price(),
            unsigned_tx: unsigned_tx.clone(),
        }))
        .await
        .context("Failed to send Propose")?;

    if let Decision::Reject = framed
        .next()
        .timeout(DECISION_TIMEOUT, || {
            tracing::debug_span!("receive decision")
        })
        .await
        .with_context(|| {
            format!(
                "Maker did not accept/reject within {} seconds.",
                DECISION_TIMEOUT.as_secs()
            )
        })?
        .context("End of stream while receiving Decision")?
        .context("Failed to decode Decision")?
        .into_decision()?
    {
        return Err(DialerFailed::Rejected);
    }

    framed
        .send(DialerMessage::DialerSignature(DialerSignature {
            dialer_signature: collab_settlement_tx.own_signature(),
        }))
        .await
        .context("Failed to send DialerSignature")?;

    let listener_signature = match framed.next().await {
        Some(Ok(msg)) => msg.into_listener_signature()?,
        Some(Err(_)) | None => {
            return Err(DialerFailed::AfterSendingSignature {
                unsigned_tx: unsigned_tx.clone(),
                error: anyhow!("failed to receive ListenerSignature"),
            });
        }
    };

    let collab_settlement_tx = match collab_settlement_tx
        .recv_counterparty_signature(listener_signature.listener_signature)
    {
        Ok(collab_settlement_tx) => collab_settlement_tx,
        Err(error) => {
            return Err(DialerFailed::AfterSendingSignature {
                unsigned_tx: unsigned_tx.clone(),
                error,
            });
        }
    };

    let settlement =
        collab_settlement_tx
            .finalize()
            .map_err(|e| DialerFailed::AfterSendingSignature {
                unsigned_tx: unsigned_tx.clone(),
                error: e,
            })?;

    Ok(settlement)
}

#[derive(Debug, thiserror::Error)]
pub enum DialerFailed {
    #[error("Rejected")]
    Rejected,
    #[error("Failed after sending signature")]
    AfterSendingSignature {
        unsigned_tx: Transaction,
        error: anyhow::Error,
    },
    #[error("Failed before sending signature")]
    BeforeSendingSignature { source: anyhow::Error },
}

impl From<anyhow::Error> for DialerFailed {
    fn from(source: anyhow::Error) -> Self {
        Self::BeforeSendingSignature { source }
    }
}

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
}

pub(crate) async fn emit_failed(order_id: OrderId, e: anyhow::Error, executor: &command::Executor) {
    if let Err(e) = executor
        .execute(order_id, |cfd| Ok(cfd.fail_collaborative_settlement(e)))
        .await
    {
        tracing::error!(%order_id, "Failed to execute `fail_collaborative_settlement` command: {e:#}");
    }
}
