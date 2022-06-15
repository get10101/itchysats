use anyhow::Result;
use bdk::bitcoin;
use bdk::bitcoin::hashes::hex::FromHex;
use bdk::bitcoin::hashes::hex::ToHex;
use maia_core::secp256k1_zkp;
use model::impl_sqlx_type_display_from_str;
use rust_decimal::Decimal;
use serde::de::Error;
use serde::Deserialize;
use serde::Serialize;
use sqlx::types::uuid::adapter::Hyphenated;
use sqlx::types::Uuid;
use std::fmt;
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
