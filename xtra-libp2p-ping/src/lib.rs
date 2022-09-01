pub mod ping;
pub mod pong;
mod protocol;

/// The name of the official ipfs/libp2p ping protocol.
///
/// Using this indicates that we are wire-compatible with other libp2p/ipfs nodes.
pub const PROTOCOL: &str = "/ipfs/ping/1.0.0";

#[cfg(test)]
mod tests {
    use super::*;
    use futures::Future;
    use futures::FutureExt;
    use std::time::Duration;
    use xtra::spawn::TokioGlobalSpawnExt;
    use xtra::Actor as _;
    use xtra::Address;
    use xtra::Context;
    use xtra_libp2p::endpoint::Subscribers;
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

        let alice_to_bob_latency = {
            || {
                let alice_ping_actor = alice_ping_actor.clone();
                async move {
                    alice_ping_actor
                        .send(ping::GetLatency(bob_peer_id))
                        .map(|res| res.unwrap())
                        .await
                }
            }
        };
        let alice_to_bob_latency = retry_until_some(alice_to_bob_latency).await;

        let bob_to_alice_latency = || {
            let bob_ping_actor = bob_ping_actor.clone();
            async move {
                bob_ping_actor
                    .send(ping::GetLatency(alice_peer_id))
                    .map(|res| res.unwrap())
                    .await
            }
        };
        let bob_to_alice_latency = retry_until_some(bob_to_alice_latency).await;

        assert!(!alice_to_bob_latency.is_zero());
        assert!(!bob_to_alice_latency.is_zero());
    }

    #[allow(clippy::type_complexity)]
    fn create_endpoint_with_ping() -> (PeerId, Address<ping::Actor>, Address<Endpoint>) {
        let (endpoint_address, endpoint_context) = Context::new(None);

        let id = Keypair::generate_ed25519();
        let ping_address = ping::Actor::new(endpoint_address.clone(), Duration::from_secs(1))
            .create(None)
            .spawn_global();
        let pong_address = pong::Actor.create(None).spawn_global();

        let endpoint = Endpoint::new(
            Box::new(MemoryTransport::default),
            id.clone(),
            Duration::from_secs(10),
            [(PROTOCOL, pong_address.into())],
            Subscribers::new(vec![ping_address.clone().into()], vec![], vec![], vec![]),
        );

        #[allow(clippy::disallowed_methods)]
        tokio::spawn(endpoint_context.run(endpoint));

        (id.public().to_peer_id(), ping_address, endpoint_address)
    }

    async fn retry_until_some<F, FUT, T>(mut fut: F) -> T
    where
        F: FnMut() -> FUT,
        FUT: Future<Output = Option<T>>,
    {
        loop {
            match fut().await {
                Some(t) => return t,
                None => tokio_extras::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }
}
