use crate::impl_sqlx_type_display_from_str;
use crate::impl_sqlx_type_integer;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use bdk::bitcoin;
use bdk::bitcoin::hashes::hex::FromHex;
use bdk::bitcoin::hashes::hex::ToHex;
use bdk::bitcoin::Amount;
use bdk::bitcoin::SignedAmount;
use maia_core::secp256k1_zkp;
use model::olivia::EVENT_TIME_FORMAT;
use rust_decimal::Decimal;
use serde::de::Error;
use serde::Deserialize;
use serde::Serialize;
use sqlx::types::uuid::fmt::Hyphenated;
use sqlx::types::Uuid;
use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;
use time::OffsetDateTime;
use time::PrimitiveDateTime;

pub type OfferId = OrderId;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct OrderId(Hyphenated);

impl Serialize for OrderId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for OrderId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let order_id = String::deserialize(deserializer)?;
        let order_id = order_id.parse::<Uuid>().map_err(D::Error::custom)?;

        Ok(Self(order_id.hyphenated()))
    }
}

impl Default for OrderId {
    fn default() -> Self {
        Self(Uuid::new_v4().hyphenated())
    }
}

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<model::OrderId> for OrderId {
    fn from(id: model::OrderId) -> Self {
        OrderId(Uuid::from(id).hyphenated())
    }
}

impl From<OrderId> for model::OrderId {
    fn from(id: OrderId) -> Self {
        let id = Uuid::from_str(id.0.to_string().as_str())
            .expect("Safe conversion from one uuid format to another");
        model::OrderId::from(id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SecretKey(secp256k1_zkp::SecretKey);

impl fmt::Display for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display_secret())
    }
}

impl FromStr for SecretKey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let sk = secp256k1_zkp::SecretKey::from_str(s)?;
        Ok(Self(sk))
    }
}

impl From<SecretKey> for secp256k1_zkp::SecretKey {
    fn from(sk: SecretKey) -> Self {
        sk.0
    }
}

impl From<secp256k1_zkp::SecretKey> for SecretKey {
    fn from(key: secp256k1_zkp::SecretKey) -> Self {
        Self(key)
    }
}

impl_sqlx_type_display_from_str!(SecretKey);

/// Role in the Cfd
#[derive(Debug, Copy, Clone, PartialEq, sqlx::Type)]
pub enum Role {
    Maker,
    Taker,
}

impl From<model::Role> for Role {
    fn from(role: model::Role) -> Self {
        match role {
            model::Role::Maker => Role::Maker,
            model::Role::Taker => Role::Taker,
        }
    }
}

impl From<Role> for model::Role {
    fn from(role: Role) -> Self {
        match role {
            Role::Maker => model::Role::Maker,
            Role::Taker => model::Role::Taker,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PublicKey(bitcoin::util::key::PublicKey);

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for PublicKey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let pk = bitcoin::util::key::PublicKey::from_str(s)?;
        Ok(Self(pk))
    }
}

impl From<PublicKey> for bitcoin::util::key::PublicKey {
    fn from(pk: PublicKey) -> Self {
        pk.0
    }
}

impl From<bitcoin::util::key::PublicKey> for PublicKey {
    fn from(pk: bitcoin::util::key::PublicKey) -> Self {
        Self(pk)
    }
}

impl_sqlx_type_display_from_str!(PublicKey);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdaptorSignature(secp256k1_zkp::EcdsaAdaptorSignature);

impl fmt::Display for AdaptorSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for AdaptorSignature {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let sk = secp256k1_zkp::EcdsaAdaptorSignature::from_str(s)?;
        Ok(Self(sk))
    }
}

impl From<AdaptorSignature> for secp256k1_zkp::EcdsaAdaptorSignature {
    fn from(sk: AdaptorSignature) -> Self {
        sk.0
    }
}
impl From<secp256k1_zkp::EcdsaAdaptorSignature> for AdaptorSignature {
    fn from(sk: secp256k1_zkp::EcdsaAdaptorSignature) -> Self {
        Self(sk)
    }
}

impl_sqlx_type_display_from_str!(AdaptorSignature);

#[derive(Clone, Debug, PartialEq)]
pub struct Transaction(bitcoin::Transaction);

impl fmt::Display for Transaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let consensus_encoded = bitcoin::consensus::encode::serialize(&self.0);
        consensus_encoded.to_hex().fmt(f)
    }
}

impl FromStr for Transaction {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = Vec::from_hex(s)?;
        let tx = bitcoin::consensus::encode::deserialize(hex.as_slice())?;
        Ok(Self(tx))
    }
}

impl From<Transaction> for bitcoin::Transaction {
    fn from(tx: Transaction) -> Self {
        tx.0
    }
}

impl From<bitcoin::Transaction> for Transaction {
    fn from(tx: bitcoin::Transaction) -> Self {
        Self(tx)
    }
}

impl_sqlx_type_display_from_str!(Transaction);

/// Represents "quantity" or "contract size" in Cfd terms
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd)]
pub struct Usd(Decimal);

impl fmt::Display for Usd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.round_dp(2).fmt(f)
    }
}

impl FromStr for Usd {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let dec = Decimal::from_str(s)?;
        Ok(Usd(dec))
    }
}

impl From<model::Usd> for Usd {
    fn from(usd: model::Usd) -> Self {
        Self(usd.into_decimal())
    }
}

impl From<Usd> for model::Usd {
    fn from(usd: Usd) -> Self {
        model::Usd::new(usd.0)
    }
}

impl_sqlx_type_display_from_str!(Usd);

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Price(Decimal);

impl From<model::Price> for Price {
    fn from(price: model::Price) -> Self {
        Self(price.into_decimal())
    }
}

impl From<Price> for model::Price {
    fn from(price: Price) -> Self {
        model::Price::new(price.0).expect("Self conversion from one Price to another should work")
    }
}

#[cfg(test)]
impl From<Decimal> for Price {
    fn from(price: Decimal) -> Self {
        Self(price)
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for Price {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let dec = Decimal::from_str(s)?;
        Ok(Price(dec))
    }
}

impl_sqlx_type_display_from_str!(Price);

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, sqlx::Type)]
#[sqlx(transparent)]
pub struct Leverage(u8);

impl fmt::Display for Leverage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let leverage = self.0;

        write!(f, "x{leverage}")
    }
}

impl From<model::Leverage> for Leverage {
    fn from(leverage: model::Leverage) -> Self {
        Self(leverage.get())
    }
}

impl From<Leverage> for model::Leverage {
    fn from(leverage: Leverage) -> Self {
        model::Leverage::new(leverage.0)
            .expect("Self conversion from one leverage to the other should work")
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, sqlx::Type)]
pub enum Position {
    Long,
    Short,
}

impl From<model::Position> for Position {
    fn from(position: model::Position) -> Self {
        match position {
            model::Position::Long => Position::Long,
            model::Position::Short => Position::Short,
        }
    }
}

impl From<Position> for model::Position {
    fn from(position: Position) -> Self {
        match position {
            Position::Long => model::Position::Long,
            Position::Short => model::Position::Short,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct Identity(x25519_dalek::PublicKey);

impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = hex::encode(self.0.as_bytes());

        write!(f, "{hex}")
    }
}

impl FromStr for Identity {
    type Err = hex::FromHexError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut key = [0u8; 32];

        hex::decode_to_slice(s, &mut key)?;

        Ok(Self(key.into()))
    }
}

impl_sqlx_type_display_from_str!(Identity);

impl From<Identity> for model::Identity {
    fn from(model: Identity) -> Self {
        model::Identity::new(model.0)
    }
}
impl From<model::Identity> for Identity {
    fn from(model: model::Identity) -> Self {
        Self(model.pk())
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, sqlx::Type)]
#[sqlx(transparent)]
pub struct Timestamp(i64);

impl From<Timestamp> for model::Timestamp {
    fn from(t: Timestamp) -> Self {
        model::Timestamp::new(t.0)
    }
}

impl From<model::Timestamp> for Timestamp {
    fn from(t: model::Timestamp) -> Self {
        Self(t.seconds())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FundingRate(Decimal);

impl fmt::Display for FundingRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for FundingRate {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let dec = Decimal::from_str(s)?;
        Ok(FundingRate(dec))
    }
}

impl From<FundingRate> for model::FundingRate {
    fn from(f: FundingRate) -> Self {
        model::FundingRate::new(f.0)
            .expect("Self conversion from one funding rate to the other should work")
    }
}

impl From<model::FundingRate> for FundingRate {
    fn from(f: model::FundingRate) -> Self {
        Self(f.to_decimal())
    }
}

impl_sqlx_type_display_from_str!(FundingRate);

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct OpeningFee(Amount);

impl fmt::Display for OpeningFee {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.as_sat().fmt(f)
    }
}

impl FromStr for OpeningFee {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let amount_sat: u64 = s.parse()?;
        Ok(OpeningFee(Amount::from_sat(amount_sat)))
    }
}

impl From<OpeningFee> for model::OpeningFee {
    fn from(f: OpeningFee) -> Self {
        model::OpeningFee::new(f.0)
    }
}

impl From<model::OpeningFee> for OpeningFee {
    fn from(f: model::OpeningFee) -> Self {
        Self(f.to_inner())
    }
}

impl_sqlx_type_display_from_str!(OpeningFee);

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct TxFeeRate(NonZeroU32);

impl TxFeeRate {
    pub fn new(fee_rate: NonZeroU32) -> Self {
        Self(fee_rate)
    }
}

impl fmt::Display for TxFeeRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for TxFeeRate {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let fee_sat = s.parse()?;
        Ok(TxFeeRate(fee_sat))
    }
}

impl From<TxFeeRate> for model::TxFeeRate {
    fn from(fee: TxFeeRate) -> Self {
        model::TxFeeRate::new(fee.0)
    }
}

impl From<model::TxFeeRate> for TxFeeRate {
    fn from(fee: model::TxFeeRate) -> Self {
        Self(fee.inner())
    }
}

impl_sqlx_type_display_from_str!(TxFeeRate);

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Txid(bitcoin::Txid);

impl Txid {
    pub fn new(txid: bitcoin::Txid) -> Self {
        Self(txid)
    }
}

impl fmt::Display for Txid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for Txid {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let txid = bdk::bitcoin::Txid::from_str(s)?;
        Ok(Self(txid))
    }
}

impl From<Txid> for bitcoin::Txid {
    fn from(txid: Txid) -> Self {
        txid.0
    }
}

impl From<bitcoin::Txid> for Txid {
    fn from(txid: bitcoin::Txid) -> Txid {
        Self(txid)
    }
}

impl_sqlx_type_display_from_str!(Txid);

#[derive(Debug, Clone, Copy, sqlx::Type, PartialEq)]
#[sqlx(transparent)]
pub struct Vout(u32);

impl Vout {
    pub fn new(vout: u32) -> Vout {
        Self(vout)
    }
}

impl From<Vout> for u32 {
    fn from(vout: Vout) -> Self {
        vout.0
    }
}

impl From<model::Vout> for Vout {
    fn from(vout: model::Vout) -> Self {
        Self(vout.inner())
    }
}

impl From<Vout> for model::Vout {
    fn from(vout: Vout) -> Self {
        model::Vout::new(vout.0)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Fees(SignedAmount);

impl From<Fees> for SignedAmount {
    fn from(fees: Fees) -> Self {
        fees.0
    }
}

impl TryFrom<i64> for Fees {
    type Error = anyhow::Error;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Ok(Self(SignedAmount::from_sat(value)))
    }
}

impl From<SignedAmount> for Fees {
    fn from(s: SignedAmount) -> Self {
        Self(s)
    }
}

impl From<&Fees> for i64 {
    fn from(fees: &Fees) -> Self {
        fees.0.as_sat() as i64
    }
}

impl From<model::Fees> for Fees {
    fn from(fee: model::Fees) -> Self {
        Self(fee.inner())
    }
}

impl From<Fees> for model::Fees {
    fn from(fee: Fees) -> Self {
        model::Fees::new(fee.0)
    }
}

impl_sqlx_type_integer!(Fees);

#[derive(Debug, Clone, Copy)]
pub struct Contracts(u64);

impl From<Contracts> for u64 {
    fn from(contracts: Contracts) -> Self {
        contracts.0
    }
}

impl From<u64> for Contracts {
    fn from(contracts: u64) -> Self {
        Self(contracts)
    }
}

impl TryFrom<i64> for Contracts {
    type Error = anyhow::Error;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        let contracts = u64::try_from(value)?;

        Ok(Self(contracts))
    }
}

impl From<&Contracts> for i64 {
    fn from(contracts: &Contracts) -> Self {
        contracts.0 as i64
    }
}

impl_sqlx_type_integer!(Contracts);

impl From<model::Contracts> for Contracts {
    fn from(contracts: model::Contracts) -> Self {
        Self(contracts.into())
    }
}

impl From<Contracts> for model::Contracts {
    fn from(contracts: Contracts) -> Self {
        model::Contracts::new(contracts.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Payout(Amount);

impl Payout {
    pub fn new(amount: Amount) -> Self {
        Self(amount)
    }
}

impl From<Payout> for SignedAmount {
    fn from(payout: Payout) -> Self {
        payout.0.to_signed().expect("Amount to fit in SignedAmount")
    }
}

impl TryFrom<i64> for Payout {
    type Error = anyhow::Error;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        let sats = u64::try_from(value)?;

        Ok(Self(Amount::from_sat(sats)))
    }
}

impl From<&Payout> for i64 {
    fn from(payout: &Payout) -> Self {
        payout.0.as_sat() as i64
    }
}

impl From<model::Payout> for Payout {
    fn from(payout: model::Payout) -> Self {
        Self(payout.inner())
    }
}

impl From<Payout> for model::Payout {
    fn from(payout: Payout) -> Self {
        model::Payout::new(payout.0)
    }
}

impl_sqlx_type_integer!(Payout);

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct PeerId(libp2p_core::PeerId);

impl From<libp2p_core::PeerId> for PeerId {
    fn from(peer_id: libp2p_core::PeerId) -> Self {
        Self(peer_id)
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for PeerId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let peer_id = libp2p_core::PeerId::from_str(s)?;
        Ok(Self(peer_id))
    }
}

impl From<model::libp2p::PeerId> for PeerId {
    fn from(id: model::libp2p::PeerId) -> Self {
        Self(id.inner())
    }
}

impl From<PeerId> for model::libp2p::PeerId {
    fn from(peerid: PeerId) -> Self {
        model::libp2p::PeerId::from(peerid.0)
    }
}

impl_sqlx_type_display_from_str!(PeerId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BitMexPriceEventId {
    /// The timestamp this price event refers to.
    timestamp: OffsetDateTime,
    digits: usize,
}

impl fmt::Display for BitMexPriceEventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "/x/BitMEX/BXBT/{}.price?n={}",
            self.timestamp
                .format(&EVENT_TIME_FORMAT)
                .expect("should always format and we can't return an error here"),
            self.digits
        )
    }
}

impl FromStr for BitMexPriceEventId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let remaining = s.trim_start_matches("/x/BitMEX/BXBT/");
        let (timestamp, rest) = remaining.split_at(19);
        let digits = rest.trim_start_matches(".price?n=");

        Ok(Self {
            timestamp: PrimitiveDateTime::parse(timestamp, &EVENT_TIME_FORMAT)
                .with_context(|| format!("Failed to parse {timestamp} as timestamp"))?
                .assume_utc(),
            digits: digits.parse()?,
        })
    }
}

impl From<model::olivia::BitMexPriceEventId> for BitMexPriceEventId {
    fn from(id: model::olivia::BitMexPriceEventId) -> Self {
        Self {
            timestamp: id.timestamp(),
            digits: id.digits(),
        }
    }
}

impl From<BitMexPriceEventId> for model::olivia::BitMexPriceEventId {
    fn from(id: BitMexPriceEventId) -> Self {
        model::olivia::BitMexPriceEventId::new(id.timestamp, id.digits)
    }
}

impl_sqlx_type_display_from_str!(BitMexPriceEventId);

/// The type of failed CFD.
#[derive(Debug, Clone, Copy)]
pub enum FailedKind {
    OfferRejected,
    ContractSetupFailed,
}

impl fmt::Display for FailedKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            FailedKind::OfferRejected => "OfferRejected",
            FailedKind::ContractSetupFailed => "ContractSetupFailed",
        };

        s.fmt(f)
    }
}

impl FromStr for FailedKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let kind = match s {
            "OfferRejected" => FailedKind::OfferRejected,
            "ContractSetupFailed" => FailedKind::ContractSetupFailed,
            other => bail!("Not a failed CFD Kind: {other}"),
        };

        Ok(kind)
    }
}

impl From<FailedKind> for model::FailedKind {
    fn from(kind: FailedKind) -> Self {
        match kind {
            FailedKind::OfferRejected => model::FailedKind::OfferRejected,
            FailedKind::ContractSetupFailed => model::FailedKind::ContractSetupFailed,
        }
    }
}

impl_sqlx_type_display_from_str!(FailedKind);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Settlement {
    Collaborative {
        txid: Txid,
        vout: Vout,
        payout: Payout,
        price: Price,
    },
    Cet {
        commit_txid: Txid,
        txid: Txid,
        vout: Vout,
        payout: Payout,
        price: Price,
    },
    Refund {
        commit_txid: Txid,
        txid: Txid,
        vout: Vout,
        payout: Payout,
    },
}

impl From<Settlement> for model::Settlement {
    fn from(settlement: Settlement) -> Self {
        match settlement {
            Settlement::Collaborative {
                txid,
                vout,
                payout,
                price,
            } => model::Settlement::Collaborative {
                txid: txid.into(),
                vout: vout.into(),
                payout: payout.into(),
                price: price.into(),
            },
            Settlement::Cet {
                commit_txid,
                txid,
                vout,
                payout,
                price,
            } => model::Settlement::Cet {
                commit_txid: commit_txid.into(),
                txid: txid.into(),
                vout: vout.into(),
                payout: payout.into(),
                price: price.into(),
            },
            Settlement::Refund {
                commit_txid,
                txid,
                vout,
                payout,
            } => model::Settlement::Refund {
                commit_txid: commit_txid.into(),
                txid: txid.into(),
                vout: vout.into(),
                payout: payout.into(),
            },
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, sqlx::Type)]
pub enum FeeFlow {
    LongPaysShort,
    ShortPaysLong,
    None,
}

pub fn into_complete_fee(
    complete_fee_flow: Option<FeeFlow>,
    complete_fee: Option<i64>,
) -> Option<model::CompleteFee> {
    match (complete_fee_flow, complete_fee) {
        (Some(fee_flow), Some(sats)) => match fee_flow {
            FeeFlow::LongPaysShort => Some(model::CompleteFee::LongPaysShort(Amount::from_sat(
                sats as u64,
            ))),
            FeeFlow::ShortPaysLong => Some(model::CompleteFee::ShortPaysLong(Amount::from_sat(
                sats as u64,
            ))),
            FeeFlow::None => Some(model::CompleteFee::None),
        },
        (None, None) => None,
        (complete_fee_flow, complete_fee) => {
            tracing::warn!(
                ?complete_fee_flow,
                ?complete_fee,
                "Weird combination of complete_fee in database"
            );
            None
        }
    }
}

pub fn into_complete_fee_and_flow(
    complete_fee: Option<model::CompleteFee>,
) -> (Option<i64>, Option<FeeFlow>) {
    match complete_fee {
        None => (None, None),
        Some(complete_fee) => match complete_fee {
            model::CompleteFee::LongPaysShort(amount) => {
                (Some(amount.as_sat() as i64), Some(FeeFlow::LongPaysShort))
            }
            model::CompleteFee::ShortPaysLong(amount) => {
                (Some(amount.as_sat() as i64), Some(FeeFlow::ShortPaysLong))
            }
            model::CompleteFee::None => (Some(0), Some(FeeFlow::None)),
        },
    }
}

/// Trading pair of the Cfd
#[derive(Debug, Copy, Clone, PartialEq, sqlx::Type)]
pub enum ContractSymbol {
    BtcUsd,
    EthUsd,
}

impl From<model::ContractSymbol> for ContractSymbol {
    fn from(contract_symbol: model::ContractSymbol) -> Self {
        match contract_symbol {
            model::ContractSymbol::BtcUsd => ContractSymbol::BtcUsd,
            model::ContractSymbol::EthUsd => ContractSymbol::EthUsd,
        }
    }
}

impl From<ContractSymbol> for model::ContractSymbol {
    fn from(contract_symbol: ContractSymbol) -> Self {
        match contract_symbol {
            ContractSymbol::BtcUsd => model::ContractSymbol::BtcUsd,
            ContractSymbol::EthUsd => model::ContractSymbol::EthUsd,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn into_complete_fee_and_flow_long_pays_short() {
        let model_complete_fee = model::CompleteFee::LongPaysShort(Amount::from_sat(1000));
        let (complete_fee, complete_fee_flow) =
            into_complete_fee_and_flow(Some(model_complete_fee));

        assert_eq!(complete_fee_flow, Some(FeeFlow::LongPaysShort));
        assert_eq!(complete_fee, Some(1000));
    }

    #[test]
    fn into_complete_fee_and_flow_short_pays_long() {
        let model_complete_fee = model::CompleteFee::ShortPaysLong(Amount::from_sat(1000));
        let (complete_fee, complete_fee_flow) =
            into_complete_fee_and_flow(Some(model_complete_fee));

        assert_eq!(complete_fee_flow, Some(FeeFlow::ShortPaysLong));
        assert_eq!(complete_fee, Some(1000));
    }

    #[test]
    fn into_complete_fee_and_flow_none_fee_flow() {
        let model_complete_fee = model::CompleteFee::None;
        let (complete_fee, complete_fee_flow) =
            into_complete_fee_and_flow(Some(model_complete_fee));

        assert_eq!(complete_fee_flow, Some(FeeFlow::None));
        assert_eq!(complete_fee, Some(0));
    }

    #[test]
    fn into_complete_fee_and_flow_none_fee_none() {
        let (complete_fee, complete_fee_flow) = into_complete_fee_and_flow(None);

        assert_eq!(complete_fee_flow, None);
        assert_eq!(complete_fee, None);
    }

    #[test]
    fn into_complete_fee_long_pays_short() {
        let complete_fee = into_complete_fee(Some(FeeFlow::LongPaysShort), Some(1000));
        assert_eq!(
            complete_fee,
            Some(model::CompleteFee::LongPaysShort(Amount::from_sat(1000)))
        );
    }

    #[test]
    fn into_complete_fee_short_pays_long() {
        let complete_fee = into_complete_fee(Some(FeeFlow::LongPaysShort), Some(1000));
        assert_eq!(
            complete_fee,
            Some(model::CompleteFee::LongPaysShort(Amount::from_sat(1000)))
        );
    }

    #[test]
    fn into_complete_fee_none_fee_flow() {
        let complete_fee = into_complete_fee(Some(FeeFlow::None), Some(0));
        assert_eq!(complete_fee, Some(model::CompleteFee::None));
    }

    #[test]
    fn into_complete_fee_none() {
        let complete_fee = into_complete_fee(None, None);
        assert_eq!(complete_fee, None);
    }

    #[test]
    fn given_weird_combination_some_none_in_database_then_none() {
        let complete_fee = into_complete_fee(Some(FeeFlow::None), None);
        assert_eq!(complete_fee, None);
    }

    #[test]
    fn given_weird_combination_none_some_in_database_then_none() {
        let complete_fee = into_complete_fee(None, Some(0));
        assert_eq!(complete_fee, None);
    }
}
