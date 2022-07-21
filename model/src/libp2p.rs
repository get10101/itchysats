use serde::Deserialize;
use serde::Serialize;
use std::fmt;
use std::str::FromStr;

#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerId(libp2p_core::PeerId);

impl fmt::Debug for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl PeerId {
    /// Value used as placeholder for legacy CFDs where no PeerId is stored.
    ///
    /// This peer-id is based on the key derived by `Seed::derive_identities` where seed is `[0u8;
    /// 256]`.
    pub const PLACEHOLDER: &'static str = "12D3KooWE1krEN7utXUFhLmLXLWPSuah5WQ7ChzUFetkMS7TRXsm";

    pub fn inner(&self) -> libp2p_core::PeerId {
        self.0
    }

    /// Placeholder used for db entries prior to using libp2p
    pub fn placeholder() -> PeerId {
        // peer id for key filled up with 0s
        Self(Self::PLACEHOLDER.parse().expect("fixed peer id to fit"))
    }

    pub fn random() -> PeerId {
        Self(libp2p_core::PeerId::random())
    }
}

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

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn libp2p_placeholder_snapshot() {
        let placeholder = PeerId::placeholder();
        assert_eq!(
            placeholder.to_string(),
            "12D3KooWE1krEN7utXUFhLmLXLWPSuah5WQ7ChzUFetkMS7TRXsm"
        );
    }

    #[test]
    fn given_placeholder_peer_id_as_string_then_equals_placeholder_identity() {
        let identity = "12D3KooWE1krEN7utXUFhLmLXLWPSuah5WQ7ChzUFetkMS7TRXsm"
            .parse::<PeerId>()
            .unwrap();

        assert_eq!(identity, PeerId::placeholder())
    }
}
