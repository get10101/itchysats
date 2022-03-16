use crate::noise::NOISE_MAX_MSG_LEN;
use crate::noise::NOISE_TAG_LEN;
use crate::olivia::BitMexPriceEventId;
use anyhow::bail;
use anyhow::Result;
use bdk::bitcoin::secp256k1::Signature;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::Address;
use bdk::bitcoin::Amount;
use bdk::bitcoin::PublicKey;
use bytes::BytesMut;
use futures::stream::SplitSink;
use futures::stream::SplitStream;
use maia::secp256k1_zkp::EcdsaAdaptorSignature;
use maia::secp256k1_zkp::SecretKey;
use maia::CfdTransactions;
use maia::PartyParams;
use maia::PunishParams;
use model::FundingRate;
use model::Leverage;
use model::MakerOffers;
use model::OpeningFee;
use model::OrderId;
use model::Origin;
use model::Position;
use model::Price;
use model::Timestamp;
use model::TradingPair;
use model::TxFeeRate;
use model::Usd;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;
use snow::TransportState;
use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::ops::RangeInclusive;
use time::Duration;
use tokio::net::TcpStream;
use tokio_util::codec::Decoder;
use tokio_util::codec::Encoder;
use tokio_util::codec::Framed;
use tokio_util::codec::LengthDelimitedCodec;

pub type Read<D, E> = SplitStream<Framed<TcpStream, EncryptedJsonCodec<D, E>>>;
pub type Write<D, E> = SplitSink<Framed<TcpStream, EncryptedJsonCodec<D, E>>, E>;

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, PartialOrd)]
pub struct Version(semver::Version);

impl Version {
    pub const LATEST: Version = Version(semver::Version::new(2, 1, 0));
    pub const V2_0_0: Version = Version(semver::Version::new(2, 0, 0));
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

pub mod taker_to_maker {
    use super::*;

    #[derive(Serialize, Deserialize, Clone, Copy)]
    #[serde(tag = "type", content = "payload")]
    #[allow(clippy::large_enum_variant)]
    pub enum Settlement {
        Propose {
            timestamp: Timestamp,
            #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
            taker: Amount,
            #[serde(with = "::bdk::bitcoin::util::amount::serde::as_btc")]
            maker: Amount,
            price: Price,
        },
        Initiate {
            sig_taker: Signature,
        },
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
#[allow(clippy::large_enum_variant)]
pub enum TakerToMaker {
    /// Deprecated, used by takers up to v0.4.7
    Hello(Version),
    HelloV2 {
        proposed_wire_version: Version,
        daemon_version: String,
    },
    TakeOrder {
        order_id: OrderId,
        quantity: Usd,
    },
    ProposeRollover {
        order_id: OrderId,
        timestamp: Timestamp,
    },
    Protocol {
        order_id: OrderId,
        msg: SetupMsg,
    },
    RolloverProtocol {
        order_id: OrderId,
        msg: RolloverMsg,
    },
    Settlement {
        order_id: OrderId,
        msg: taker_to_maker::Settlement,
    },
}

impl TakerToMaker {
    pub fn name(&self) -> &'static str {
        match self {
            TakerToMaker::TakeOrder { .. } => "TakerToMaker::TakeOrder",
            TakerToMaker::Protocol { msg, .. } => match msg {
                SetupMsg::Msg0(_) => "TakerToMaker::Protocol::Msg0",
                SetupMsg::Msg1(_) => "TakerToMaker::Protocol::Msg1",
                SetupMsg::Msg2(_) => "TakerToMaker::Protocol::Msg2",
                SetupMsg::Msg3(_) => "TakerToMaker::Protocol::Msg3",
            },
            TakerToMaker::ProposeRollover { .. } => "TakerToMaker::ProposeRollover",
            TakerToMaker::RolloverProtocol { msg, .. } => match msg {
                RolloverMsg::Msg0(_) => "TakerToMaker::RolloverProtocol::Msg0",
                RolloverMsg::Msg1(_) => "TakerToMaker::RolloverProtocol::Msg1",
                RolloverMsg::Msg2(_) => "TakerToMaker::RolloverProtocol::Msg2",
                RolloverMsg::Msg3(_) => "TakerToMaker::RolloverProtocol::Msg3",
            },
            TakerToMaker::Settlement { msg, .. } => match msg {
                taker_to_maker::Settlement::Propose { .. } => "TakerToMaker::Settlement::Propose",
                taker_to_maker::Settlement::Initiate { .. } => "TakerToMaker::Settlement::Initiate",
            },
            TakerToMaker::Hello(_) => "TakerToMaker::Hello",
            TakerToMaker::HelloV2 { .. } => "TakerToMaker::HelloV2",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
#[allow(clippy::large_enum_variant)]
pub enum MakerToTaker {
    Hello(Version),
    /// Periodically broadcast message, indicating maker's presence
    Heartbeat,
    CurrentOrder(Option<DeprecatedOrder047>),
    CurrentOffers(Option<MakerOffers>),
    ConfirmOrder(OrderId),
    RejectOrder(OrderId),
    InvalidOrderId(OrderId),
    Protocol {
        order_id: OrderId,
        msg: SetupMsg,
    },
    RolloverProtocol {
        order_id: OrderId,
        msg: RolloverMsg,
    },
    ConfirmRollover {
        order_id: OrderId,
        oracle_event_id: BitMexPriceEventId,
        tx_fee_rate: TxFeeRate,
        funding_rate: FundingRate,
    },
    RejectRollover(OrderId),
    Settlement {
        order_id: OrderId,
        msg: maker_to_taker::Settlement,
    },
}

/// Legacy wire struct to retain backwards compatibility on the wire with version 0.4.7
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct DeprecatedOrder047 {
    pub id: OrderId,
    pub trading_pair: TradingPair,
    #[serde(rename = "position")]
    pub position_maker: Position,
    pub price: Price,
    pub min_quantity: Usd,
    pub max_quantity: Usd,
    #[serde(rename = "leverage")]
    pub leverage_taker: Leverage,
    pub creation_timestamp: Timestamp,
    pub settlement_interval: Duration,
    pub liquidation_price: Price,
    pub origin: Origin,
    pub oracle_event_id: BitMexPriceEventId,
    pub tx_fee_rate: TxFeeRate,
    pub funding_rate: FundingRate,
    pub opening_fee: OpeningFee,
}

impl MakerToTaker {
    pub fn name(&self) -> &'static str {
        match self {
            MakerToTaker::Hello(_) => "MakerToTaker::Hello",
            MakerToTaker::Heartbeat { .. } => "MakerToTaker::Heartbeat",
            MakerToTaker::CurrentOrder(_) => "MakerToTaker::CurrentOrder",
            MakerToTaker::CurrentOffers(_) => "MakerToTaker::CurrentOffers",
            MakerToTaker::ConfirmOrder(_) => "MakerToTaker::ConfirmOrder",
            MakerToTaker::RejectOrder(_) => "MakerToTaker::RejectOrder",
            MakerToTaker::InvalidOrderId(_) => "MakerToTaker::InvalidOrderId",
            MakerToTaker::Protocol { msg, .. } => match msg {
                SetupMsg::Msg0(_) => "MakerToTaker::Protocol::Msg0",
                SetupMsg::Msg1(_) => "MakerToTaker::Protocol::Msg1",
                SetupMsg::Msg2(_) => "MakerToTaker::Protocol::Msg2",
                SetupMsg::Msg3(_) => "MakerToTaker::Protocol::Msg3",
            },
            MakerToTaker::ConfirmRollover { .. } => "MakerToTaker::ConfirmRollover",
            MakerToTaker::RejectRollover(_) => "MakerToTaker::RejectRollover",
            MakerToTaker::RolloverProtocol { msg, .. } => match msg {
                RolloverMsg::Msg0(_) => "MakerToTaker::RolloverProtocol::Msg0",
                RolloverMsg::Msg1(_) => "MakerToTaker::RolloverProtocol::Msg1",
                RolloverMsg::Msg2(_) => "MakerToTaker::RolloverProtocol::Msg2",
                RolloverMsg::Msg3(_) => "MakerToTaker::RolloverProtocol::Msg3",
            },
            MakerToTaker::Settlement { msg, .. } => match msg {
                maker_to_taker::Settlement::Confirm => "MakerToTaker::Settlement::Confirm",
                maker_to_taker::Settlement::Reject => "MakerToTaker::Settlement::Reject",
            },
        }
    }
}

pub mod maker_to_taker {
    use super::*;

    #[derive(Debug, Serialize, Deserialize, Clone, Copy)]
    #[serde(tag = "type", content = "payload")]
    pub enum Settlement {
        Confirm,
        Reject,
    }
}

/// A codec that can decode encrypted JSON into the type `D` and encode `E` to encrypted JSON.
pub struct EncryptedJsonCodec<D, E> {
    _type: PhantomData<(D, E)>,
    inner: LengthDelimitedCodec,
    transport_state: TransportState,
}

impl<D, E> EncryptedJsonCodec<D, E> {
    pub fn new(transport_state: TransportState) -> Self {
        Self {
            _type: PhantomData,
            inner: LengthDelimitedCodec::new(),
            transport_state,
        }
    }
}

impl<D, E> Decoder for EncryptedJsonCodec<D, E>
where
    D: DeserializeOwned,
{
    type Item = D;
    type Error = anyhow::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let bytes = match self.inner.decode(src)? {
            None => return Ok(None),
            Some(bytes) => bytes,
        };

        let decrypted = bytes
            .chunks(NOISE_MAX_MSG_LEN as usize)
            .map(|chunk| {
                let mut buf = vec![0u8; chunk.len() - NOISE_TAG_LEN as usize];
                self.transport_state.read_message(chunk, &mut *buf)?;
                Ok(buf)
            })
            .collect::<Result<Vec<Vec<u8>>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<u8>>();

        let item = serde_json::from_slice(&decrypted)?;

        Ok(Some(item))
    }
}

impl<D, E> Encoder<E> for EncryptedJsonCodec<D, E>
where
    E: Serialize,
{
    type Error = anyhow::Error;

    fn encode(&mut self, item: E, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let bytes = serde_json::to_vec(&item)?;

        let encrypted = bytes
            .chunks((NOISE_MAX_MSG_LEN - NOISE_TAG_LEN) as usize)
            .map(|chunk| {
                let mut buf = vec![0u8; chunk.len() + NOISE_TAG_LEN as usize];
                self.transport_state.write_message(chunk, &mut *buf)?;
                Ok(buf)
            })
            .collect::<Result<Vec<Vec<u8>>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<u8>>();

        self.inner.encode(encrypted.into(), dst)?;

        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
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
    /// Message acknowledging that we received everything
    ///
    /// Simple ACK message used at the end of the message exchange to ensure that both parties sent
    /// and received everything and we did not run into timeouts on the other side.
    /// This is used to avoid one party publishing the lock transaction while the other party ran
    /// into a timeout.
    Msg3(Msg3),
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

    pub fn try_into_msg3(self) -> Result<Msg3> {
        if let Self::Msg3(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg3")
        }
    }
}

#[derive(Serialize, Deserialize)]
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

#[derive(Serialize, Deserialize)]
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

#[derive(Serialize, Deserialize)]
pub struct Msg2 {
    pub signed_lock: PartiallySignedTransaction, // TODO: Use binary representation
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Msg3;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum RolloverMsg {
    Msg0(RolloverMsg0),
    Msg1(RolloverMsg1),
    Msg2(RolloverMsg2),
    Msg3(RolloverMsg3),
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct RolloverMsg0 {
    pub revocation_pk: PublicKey,
    pub publish_pk: PublicKey,
}

impl RolloverMsg {
    pub fn try_into_msg0(self) -> Result<RolloverMsg0> {
        if let Self::Msg0(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg0")
        }
    }

    pub fn try_into_msg1(self) -> Result<RolloverMsg1> {
        if let Self::Msg1(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg1")
        }
    }

    pub fn try_into_msg2(self) -> Result<RolloverMsg2> {
        if let Self::Msg2(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg2")
        }
    }

    pub fn try_into_msg3(self) -> Result<RolloverMsg3> {
        if let Self::Msg3(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg3")
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct RolloverMsg1 {
    pub commit: EcdsaAdaptorSignature,
    pub cets: HashMap<String, Vec<(RangeInclusive<u64>, EcdsaAdaptorSignature)>>,
    pub refund: Signature,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct RolloverMsg2 {
    pub revocation_sk: SecretKey,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct RolloverMsg3;

impl From<CfdTransactions> for RolloverMsg1 {
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
