pub mod ping;
pub mod pong;
mod protocol;

/// The name of the official ipfs/libp2p ping protocol.
///
/// Using this indicates that we are wire-compatible with other libp2p/ipfs nodes.
pub const PROTOCOL_NAME: &str = "/ipfs/ping/1.0.0";

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use xtra::message_channel::StrongMessageChannel;
    use xtra::spawn::TokioGlobalSpawnExt;
    use xtra::Actor as _;
    use xtra::Address;
    use xtra::Context;
    use xtra_libp2p::libp2p::identity::Keypair;
    use xtra_libp2p::libp2p::multiaddr::Protocol;
    use xtra_libp2p::libp2p::transport::MemoryTransport;
    use xtra_libp2p::libp2p::Multiaddr;
    use xtra_libp2p::libp2p::PeerId;
    use xtra_libp2p::Connect;
    use xtra_libp2p::Endpoint;
    use xtra_libp2p::ListenOn;

    #[tokio::test]
    async fn latency_to_peer_is_recorded() {
        tracing_subscriber::fmt()
            .with_env_filter("xtra_libp2p_ping=trace")
            .with_test_writer()
            .init();

        let (alice_peer_id, alice_ping_actor, alice_endpoint) = create_endpoint_with_ping();
        let (bob_peer_id, bob_ping_actor, bob_endpoint) = create_endpoint_with_ping();

        alice_endpoint
            .send(ListenOn(Multiaddr::empty().with(Protocol::Memory(1000))))
            .await
            .unwrap();
        bob_endpoint
            .send(Connect(
                Multiaddr::empty()
                    .with(Protocol::Memory(1000))
                    .with(Protocol::P2p(alice_peer_id.into())),
            ))
            .await
            .unwrap()
            .unwrap();

        tokio::time::sleep(Duration::from_secs(2)).await;

        let alice_to_bob_latency = alice_ping_actor
            .send(ping::GetLatency(bob_peer_id))
            .await
            .unwrap()
            .unwrap();
        let bob_to_alice_latency = bob_ping_actor
            .send(ping::GetLatency(alice_peer_id))
            .await
            .unwrap()
            .unwrap();

        assert!(!alice_to_bob_latency.is_zero());
        assert!(!bob_to_alice_latency.is_zero());
    }

    fn create_endpoint_with_ping() -> (PeerId, Address<ping::Actor>, Address<Endpoint>) {
        let (endpoint_address, endpoint_context) = Context::new(None);

        let id = Keypair::generate_ed25519();
        let ping_address = ping::Actor::new(endpoint_address.clone(), Duration::from_secs(1))
            .create(None)
            .spawn_global();
        let pong_address = pong::Actor::default().create(None).spawn_global();

        let endpoint = Endpoint::new(
            MemoryTransport::default(),
            id.clone(),
            Duration::from_secs(10),
            [(PROTOCOL_NAME, pong_address.clone_channel())],
        );

        #[allow(clippy::disallowed_methods)]
        tokio::spawn(endpoint_context.run(endpoint));

        (id.public().to_peer_id(), ping_address, endpoint_address)
    }
}
