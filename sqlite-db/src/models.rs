use anyhow::Result;
use bdk::bitcoin;
use bdk::bitcoin::hashes::hex::FromHex;
use bdk::bitcoin::hashes::hex::ToHex;
use bdk::bitcoin::Amount;
use maia_core::secp256k1_zkp;
use model::impl_sqlx_type_display_from_str;
use rust_decimal::Decimal;
use serde::de::Error;
use serde::Deserialize;
use serde::Serialize;
use sqlx::types::uuid::adapter::Hyphenated;
use sqlx::types::Uuid;
use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;

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
        let uuid = String::deserialize(deserializer)?;
        let uuid = uuid.parse::<Uuid>().map_err(D::Error::custom)?;

        Ok(Self(uuid.to_hyphenated()))
    }
}

impl Default for OrderId {
    fn default() -> Self {
        Self(Uuid::new_v4().to_hyphenated())
    }
}

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<model::OrderId> for OrderId {
    fn from(id: model::OrderId) -> Self {
        OrderId(Uuid::from(id).to_hyphenated())
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
pub struct SecretKey(secp256k1_zkp::key::SecretKey);

impl fmt::Display for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for SecretKey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let sk = secp256k1_zkp::key::SecretKey::from_str(s)?;
        Ok(Self(sk))
    }
}

impl From<SecretKey> for secp256k1_zkp::key::SecretKey {
    fn from(sk: SecretKey) -> Self {
        sk.0
    }
}

impl From<secp256k1_zkp::key::SecretKey> for SecretKey {
    fn from(key: secp256k1_zkp::key::SecretKey) -> Self {
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
