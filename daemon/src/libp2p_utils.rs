use anyhow::ensure;
use anyhow::Context;
use anyhow::Result;
use std::net::IpAddr;
use std::net::SocketAddr;

use libp2p_core::Multiaddr;
use libp2p_core::PeerId;

/// Creates MultiAddr from SocketAddr and PeerId
pub fn create_connect_tcp_multiaddr(
    socket_addr: &SocketAddr,
    peer_id: PeerId,
) -> Result<Multiaddr> {
    let ip = socket_addr.ip();
    let port = socket_addr.port();
    ensure!(socket_addr.is_ipv4(), "only ipv4 is supported");

    format!("/ip4/{ip}/tcp/{port}/p2p/{peer_id}")
        .parse::<Multiaddr>()
        .with_context(|| "failed to construct multiaddr")
}

/// Construct a Multiaddr that can dial in to other party given their MultiAddr
/// and PeerId
pub fn create_connect_multiaddr(
    listener_multiaddr: &Multiaddr,
    listener_peer_id: &PeerId,
) -> Result<Multiaddr> {
    format!("{listener_multiaddr}/p2p/{listener_peer_id}")
        .parse::<Multiaddr>()
        .with_context(|| "failed to construct multiaddr")
}

/// Creates MultiAddr from SocketAddr
pub fn create_listen_tcp_multiaddr(ip: &IpAddr, port: u16) -> Result<Multiaddr> {
    ensure!(ip.is_ipv4(), "only ipv4 is supported");

    format!("/ip4/{ip}/tcp/{port}")
        .parse::<Multiaddr>()
        .with_context(|| "failed to construct multiaddr")
}

/// Determine whether to use libp2p or fallback to a legacy protocol
pub fn can_use_libp2p(cfd: &model::Cfd) -> bool {
    // Our abitily to kick-off a libp2p version of protocol is constrained by
    // having previously stored the PeerId of the counterparty. Otherwise we
    // can't dial in to them using libp2p.
    cfd.counterparty_peer_id().is_some()
}
