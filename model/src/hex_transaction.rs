use super::*;
use bdk::bitcoin;
use bdk::bitcoin::Transaction;
use serde::Deserializer;
use serde::Serializer;

pub fn serialize<S: Serializer>(value: &Transaction, serializer: S) -> Result<S::Ok, S::Error> {
    let bytes = bitcoin::consensus::serialize(value);
    let hex_str = hex::encode(bytes);
    serializer.serialize_str(hex_str.as_str())
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<Transaction, D::Error>
where
    D: Deserializer<'de>,
{
    let hex = String::deserialize(deserializer).map_err(D::Error::custom)?;
    let bytes = hex::decode(hex).map_err(D::Error::custom)?;
    let tx = bitcoin::consensus::deserialize(&bytes).map_err(D::Error::custom)?;
    Ok(tx)
}

pub mod opt {
    use super::*;

    pub fn serialize<S: Serializer>(
        value: &Option<Transaction>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            None => serializer.serialize_none(),
            Some(value) => super::serialize(value, serializer),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Transaction>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex = match Option::<String>::deserialize(deserializer).map_err(D::Error::custom)? {
            None => return Ok(None),
            Some(hex) => hex,
        };

        let bytes = hex::decode(hex).map_err(D::Error::custom)?;
        let tx = bitcoin::consensus::deserialize(&bytes).map_err(D::Error::custom)?;

        Ok(Some(tx))
    }
}
