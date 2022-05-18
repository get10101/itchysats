use crate::future_ext::FutureExt;
use crate::setup_contract;
use crate::transaction_ext::TransactionExt;
use crate::wire::CompleteFee;
use crate::wire::RolloverMsg;
use crate::wire::RolloverMsg0;
use crate::wire::RolloverMsg1;
use crate::wire::RolloverMsg2;
use crate::wire::RolloverMsg3;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use async_stream::stream;
use asynchronous_codec::Framed;
use asynchronous_codec::JsonCodec;
use bdk::bitcoin::secp256k1::schnorrsig;
use bdk::bitcoin::secp256k1::SECP256K1;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::Amount;
use bdk::bitcoin::PublicKey;
use bdk::miniscript::DescriptorTrait;
use bdk_ext::keypair;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::PeerId;
use maia::commit_descriptor;
use maia::renew_cfd_transactions;
use maia_core::secp256k1_zkp;
use maia_core::Announcement;
use maia_core::PartyParams;
use maia_core::PunishParams;
use model::calculate_payouts;
use model::olivia;
use model::olivia::BitMexPriceEventId;
use model::Cet;
use model::Dlc;
use model::FeeFlow;
use model::FundingFee;
use model::FundingRate;
use model::OrderId;
use model::Position;
use model::RevokedCommit;
use model::Role;
use model::RolloverParams;
use model::Timestamp;
use model::TxFeeRate;
use model::CET_TIMELOCK;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;
use xtra::Address;
use xtra_libp2p::Endpoint;
use xtra_libp2p::OpenSubstream;
use xtra_libp2p::Substream;

use super::PROTOCOL;

pub struct RolloverCompletedParams {
    pub dlc: Dlc,
    pub funding_fee: FundingFee,
}

// in other words, the taker
pub async fn dialer(
    endpoint: Address<Endpoint>,
    order_id: OrderId,
    counterparty: PeerId,
) -> Result<RolloverCompletedParams, DialerError> {
    let substream = endpoint
        .send(OpenSubstream::single_protocol(counterparty, PROTOCOL))
        .await
        .context("Endpoint is disconnected")?
        .context("Failed to open substream")?;
    let mut framed = asynchronous_codec::Framed::new(
        substream,
        asynchronous_codec::JsonCodec::<DialerMessage, ListenerMessage>::new(),
    );

    framed
        .send(DialerMessage::Propose(Propose {
            order_id,
            timestamp: Timestamp::now(),
        }))
        .await
        .context("Failed to send Msg0")?;

    // TODO: We will need to apply a timeout to these. Perhaps we can put a timeout generally into
    // "reading from the substream"?
    match framed
        .next()
        .await
        .context("End of stream while receiving Msg1")?
        .context("Failed to decode Msg1")?
        .into_decision()?
    {
        Decision::Confirm(_) => {
            // TODO: Add setup_contract::roll_over() invocation
            // setup_contract::roll_over(sink, stream, _, rollover_params, our_role, our_position,
            // dlc, n_payouts, complete_fee)
        }
        Decision::Reject(_) => return Err(DialerError::Rejected),
    }
    todo!("Finish this function");
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

#[derive(Serialize, Deserialize)]
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

#[derive(Serialize, Deserialize)]
pub struct Propose {
    pub order_id: OrderId,
    pub timestamp: Timestamp,
}

#[derive(Serialize, Deserialize)]
pub struct Confirm {
    pub order_id: OrderId,
    pub oracle_event_id: BitMexPriceEventId,
    pub tx_fee_rate: TxFeeRate,
    pub funding_rate: FundingRate,
    pub complete_fee: CompleteFee,
}

#[derive(Serialize, Deserialize)]
pub struct Reject {
    pub order_id: OrderId,
}
