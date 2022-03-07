use libp2p_core::multiaddr::Protocol;
use libp2p_core::Multiaddr;
use libp2p_core::PeerId;

pub trait MultiaddrExt {
    fn extract_peer_id(self) -> Option<PeerId>;
}

impl MultiaddrExt for Multiaddr {
    fn extract_peer_id(mut self) -> Option<PeerId> {
        let peer_id = match self.pop()? {
            Protocol::P2p(hash) => PeerId::from_multihash(hash).ok()?,
            _ => return None,
        };

        Some(peer_id)
    }
}
