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
    use futures::Future;
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 1000)]
    async fn latency_to_peers_is_low() {
        tracing_subscriber::fmt()
            .with_env_filter("xtra_libp2p_ping=trace")
            .with_env_filter("xtra=trace")
            .with_test_writer()
            .init();

        let (alice_peer_id, _alice_ping_actor, alice_endpoint) = create_endpoint_with_ping(true);

        alice_endpoint
            .send(ListenOn(Multiaddr::empty().with(Protocol::Memory(1000))))
            .await
            .unwrap();

        let mut bob_endpoints = Vec::new();
        for bob in 0..200 {
            let (_bob_peer_id, _bob_ping_actor, bob_endpoint) = create_endpoint_with_ping(true);
            bob_endpoints.push(bob_endpoint);

            tracing::info!(%bob, "Spawned bob")
        }

        for (bob, bob_endpoint) in bob_endpoints.iter().enumerate() {
            bob_endpoint
                .send(Connect(
                    Multiaddr::empty()
                        .with(Protocol::Memory(1000))
                        .with(Protocol::P2p(alice_peer_id.into())),
                ))
                .await
                .unwrap()
                .unwrap();

            tracing::info!(%bob, "Connected bob to Alice")
        }

        tokio::time::sleep(Duration::from_secs(60)).await;
    }

    fn create_endpoint_with_ping(
        is_pinger: bool,
    ) -> (PeerId, Option<Address<ping::Actor>>, Address<Endpoint>) {
        let (endpoint_address, endpoint_context) = Context::new(None);

        let id = Keypair::generate_ed25519();
        let ping_address = is_pinger.then(|| {
            ping::Actor::new(endpoint_address.clone(), Duration::from_secs(5))
                .create(None)
                .spawn_global()
        });
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

    async fn retry_until_some<F, FUT, T>(mut fut: F) -> T
    where
        F: FnMut() -> FUT,
        FUT: Future<Output = Option<T>>,
    {
        loop {
            match fut().await {
                Some(t) => return t,
                None => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }
}
