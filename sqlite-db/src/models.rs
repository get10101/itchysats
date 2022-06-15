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
