use anyhow::ensure;
use anyhow::Context;
use anyhow::Result;
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

/// Creates MultiAddr from SocketAddr
pub fn create_listen_tcp_multiaddr(socket_addr: &SocketAddr) -> Result<Multiaddr> {
    let ip = socket_addr.ip();
    let port = socket_addr.port();
    ensure!(socket_addr.is_ipv4(), "only ipv4 is supported");

    format!("/ip4/{ip}/tcp/{port}")
        .parse::<Multiaddr>()
        .with_context(|| "failed to construct multiaddr")
}

/// By convention we increment the port by 1 for libp2p-based connections.
///
/// The obvious drawback is that when doing blue/green deployment, we need to
/// increment/decrement ports by 2.
pub fn libp2p_socket_from_legacy_networking(legacy_addr: &SocketAddr) -> SocketAddr {
    SocketAddr::new(legacy_addr.ip(), legacy_addr.port() + 1)
}
