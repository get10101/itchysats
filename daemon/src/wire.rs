use crate::model::cfd::OrderId;
use crate::model::{OracleEventId, Usd};
use crate::Order;
use anyhow::{bail, Result};
use bdk::bitcoin::secp256k1::Signature;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::{Address, Amount, PublicKey};
use bytes::BytesMut;
use cfd_protocol::secp256k1_zkp::{EcdsaAdaptorSignature, SecretKey};
use cfd_protocol::{CfdTransactions, PartyParams, PunishParams};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::ops::RangeInclusive;
use std::time::SystemTime;
use tokio_util::codec::{Decoder, Encoder, LengthDelimitedCodec};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
#[allow(clippy::large_enum_variant)]
pub enum TakerToMaker {
    TakeOrder {
        order_id: OrderId,
        quantity: Usd,
    },
    ProposeSettlement {
        order_id: OrderId,
        timestamp: SystemTime,
        #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
        taker: Amount,
        #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
        maker: Amount,
    },
    InitiateSettlement {
        order_id: OrderId,
        sig_taker: Signature,
    },
    ProposeRollOver {
        order_id: OrderId,
        timestamp: SystemTime,
    },
    Protocol(SetupMsg),
    RollOverProtocol(RollOverMsg),
}

impl fmt::Display for TakerToMaker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TakerToMaker::TakeOrder { .. } => write!(f, "TakeOrder"),
            TakerToMaker::ProposeSettlement { .. } => write!(f, "ProposeSettlement"),
            TakerToMaker::InitiateSettlement { .. } => write!(f, "InitiateSettlement"),
            TakerToMaker::Protocol(_) => write!(f, "Protocol"),
            TakerToMaker::ProposeRollOver { .. } => write!(f, "ProposeRollOver"),
            TakerToMaker::RollOverProtocol(_) => write!(f, "RollOverProtocol"),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
#[allow(clippy::large_enum_variant)]
pub enum MakerToTaker {
    CurrentOrder(Option<Order>),
    ConfirmOrder(OrderId), // TODO: Include payout curve in "accept" message from maker
    RejectOrder(OrderId),
    ConfirmSettlement(OrderId),
    RejectSettlement(OrderId),
    InvalidOrderId(OrderId),
    Protocol(SetupMsg),
    RollOverProtocol(RollOverMsg),
    ConfirmRollOver {
        order_id: OrderId,
        oracle_event_id: OracleEventId,
    },
    RejectRollOver(OrderId),
}

impl fmt::Display for MakerToTaker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MakerToTaker::CurrentOrder(_) => write!(f, "CurrentOrder"),
            MakerToTaker::ConfirmOrder(_) => write!(f, "ConfirmOrder"),
            MakerToTaker::RejectOrder(_) => write!(f, "RejectOrder"),
            MakerToTaker::ConfirmSettlement(_) => write!(f, "ConfirmSettlement"),
            MakerToTaker::RejectSettlement(_) => write!(f, "RejectSettlement"),
            MakerToTaker::InvalidOrderId(_) => write!(f, "InvalidOrderId"),
            MakerToTaker::Protocol(_) => write!(f, "Protocol"),
            MakerToTaker::ConfirmRollOver { .. } => write!(f, "ConfirmRollOver"),
            MakerToTaker::RejectRollOver(_) => write!(f, "RejectRollOver"),
            MakerToTaker::RollOverProtocol(_) => write!(f, "RollOverProtocol"),
        }
    }
}

pub struct JsonCodec<T> {
    _type: PhantomData<T>,
    inner: LengthDelimitedCodec,
}

impl<T> JsonCodec<T> {
    pub fn new() -> Self {
        Self {
            _type: PhantomData,
            inner: LengthDelimitedCodec::new(),
        }
    }
}

impl<T> Decoder for JsonCodec<T>
where
    T: DeserializeOwned,
{
    type Item = T;
    type Error = anyhow::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let bytes = match self.inner.decode(src)? {
            None => return Ok(None),
            Some(bytes) => bytes,
        };

        let item = serde_json::from_slice(&bytes)?;

        Ok(Some(item))
    }
}

impl<T> Encoder<T> for JsonCodec<T>
where
    T: Serialize,
{
    type Error = anyhow::Error;

    fn encode(&mut self, item: T, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let bytes = serde_json::to_vec(&item)?;

        self.inner.encode(bytes.into(), dst)?;

        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum SetupMsg {
    /// Message enabling setting up lock and based on that commit, refund and cets
    ///
    /// Each party sends and receives this message.
    /// After receiving this message each party is able to construct the lock transaction.
    Msg0(Msg0),
    /// Message that ensures complete commit, cets and refund transactions
    ///
    /// Each party sends and receives this message.
    /// After receiving this message the commit, refund and cet transactions are complete.
    /// Once verified we can sign and send the lock PSBT.
    Msg1(Msg1),
    /// Message adding signature to the lock PSBT
    ///
    /// Each party sends and receives this message.
    /// Upon receiving this message from the other party we merge our signature and then the lock
    /// tx is fully signed and can be published on chain.
    Msg2(Msg2),
}

impl SetupMsg {
    pub fn try_into_msg0(self) -> Result<Msg0> {
        if let Self::Msg0(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg0")
        }
    }

    pub fn try_into_msg1(self) -> Result<Msg1> {
        if let Self::Msg1(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg1")
        }
    }

    pub fn try_into_msg2(self) -> Result<Msg2> {
        if let Self::Msg2(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg2")
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Msg0 {
    pub lock_psbt: PartiallySignedTransaction, // TODO: Use binary representation
    pub identity_pk: PublicKey,
    #[serde(with = "bdk::bitcoin::util::amount::serde::as_sat")]
    pub lock_amount: Amount,
    pub address: Address,
    pub revocation_pk: PublicKey,
    pub publish_pk: PublicKey,
}

impl From<(PartyParams, PunishParams)> for Msg0 {
    fn from((party, punish): (PartyParams, PunishParams)) -> Self {
        let PartyParams {
            lock_psbt,
            identity_pk,
            lock_amount,
            address,
        } = party;
        let PunishParams {
            revocation_pk,
            publish_pk,
        } = punish;

        Self {
            lock_psbt,
            identity_pk,
            lock_amount,
            address,
            revocation_pk,
            publish_pk,
        }
    }
}

impl From<Msg0> for (PartyParams, PunishParams) {
    fn from(msg0: Msg0) -> Self {
        let Msg0 {
            lock_psbt,
            identity_pk,
            lock_amount,
            address,
            revocation_pk,
            publish_pk,
        } = msg0;

        let party = PartyParams {
            lock_psbt,
            identity_pk,
            lock_amount,
            address,
        };
        let punish = PunishParams {
            revocation_pk,
            publish_pk,
        };

        (party, punish)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Msg1 {
    pub commit: EcdsaAdaptorSignature,
    pub cets: HashMap<String, Vec<(RangeInclusive<u64>, EcdsaAdaptorSignature)>>,
    pub refund: Signature,
}

impl From<CfdTransactions> for Msg1 {
    fn from(txs: CfdTransactions) -> Self {
        let cets = txs
            .cets
            .into_iter()
            .map(|grouped_cets| {
                (
                    grouped_cets.event.id,
                    grouped_cets
                        .cets
                        .into_iter()
                        .map(|(_, encsig, digits)| (digits.range(), encsig))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<HashMap<_, _>>();
        Self {
            commit: txs.commit.1,
            cets,
            refund: txs.refund.1,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Msg2 {
    pub signed_lock: PartiallySignedTransaction, // TODO: Use binary representation
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum RollOverMsg {
    Msg0(RollOverMsg0),
    Msg1(RollOverMsg1),
    Msg2(RollOverMsg2),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RollOverMsg0 {
    pub revocation_pk: PublicKey,
    pub publish_pk: PublicKey,
}

impl RollOverMsg {
    pub fn try_into_msg0(self) -> Result<RollOverMsg0> {
        if let Self::Msg0(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg0")
        }
    }

    pub fn try_into_msg1(self) -> Result<RollOverMsg1> {
        if let Self::Msg1(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg1")
        }
    }

    pub fn try_into_msg2(self) -> Result<RollOverMsg2> {
        if let Self::Msg2(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg2")
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RollOverMsg1 {
    pub commit: EcdsaAdaptorSignature,
    pub cets: HashMap<String, Vec<(RangeInclusive<u64>, EcdsaAdaptorSignature)>>,
    pub refund: Signature,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RollOverMsg2 {
    pub revocation_sk: SecretKey,
}

impl From<CfdTransactions> for RollOverMsg1 {
    fn from(txs: CfdTransactions) -> Self {
        let cets = txs
            .cets
            .into_iter()
            .map(|grouped_cets| {
                (
                    grouped_cets.event.id,
                    grouped_cets
                        .cets
                        .into_iter()
                        .map(|(_, encsig, digits)| (digits.range(), encsig))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<HashMap<_, _>>();
        Self {
            commit: txs.commit.1,
            cets,
            refund: txs.refund.1,
        }
    }
}
