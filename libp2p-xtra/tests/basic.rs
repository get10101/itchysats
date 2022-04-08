use anyhow::Context as _;
use anyhow::Result;
use async_trait::async_trait;
use asynchronous_codec::Bytes;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::multiaddr::Protocol;
use libp2p_core::Multiaddr;
use libp2p_xtra::libp2p::identity::Keypair;
use libp2p_xtra::libp2p::transport::MemoryTransport;
use libp2p_xtra::libp2p::PeerId;
use libp2p_xtra::Connect;
use libp2p_xtra::Disconnect;
use libp2p_xtra::Endpoint;
use libp2p_xtra::GetConnectionStats;
use libp2p_xtra::ListenOn;
use libp2p_xtra::NewInboundSubstream;
use libp2p_xtra::OpenSubstream;
use std::collections::HashSet;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::message_channel::StrongMessageChannel;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor;
use xtra::Address;
use xtra_productivity::xtra_productivity;

#[tokio::test]
async fn hello_world() {
    let alice_hello_world_handler = HelloWorld::default().create(None).spawn_global();
    let (alice_peer_id, _, _alice, bob, _) = alice_and_bob(
        [(
            "/hello-world/1.0.0",
            alice_hello_world_handler.clone_channel(),
        )],
        [],
    )
    .await;

    let bob_to_alice = bob
        .send(OpenSubstream::single_protocol(
            alice_peer_id,
            "/hello-world/1.0.0",
        ))
        .await
        .unwrap()
        .unwrap();

    let string = hello_world_dialer(bob_to_alice, "Bob").await.unwrap();

    assert_eq!(string, "Hello Bob!");
}

#[tokio::test]
async fn after_connect_see_each_other_as_connected() {
    let (alice_peer_id, bob_peer_id, alice, bob, _) = alice_and_bob([], []).await;

    let alice_stats = alice.send(GetConnectionStats).await.unwrap();
    let bob_stats = bob.send(GetConnectionStats).await.unwrap();

    assert_eq!(alice_stats.connected_peers, HashSet::from([bob_peer_id]));
    assert_eq!(bob_stats.connected_peers, HashSet::from([alice_peer_id]));
}

#[tokio::test]
async fn disconnect_is_reflected_in_stats() {
    let (_, bob_peer_id, alice, bob, _) = alice_and_bob([], []).await;

    alice.send(Disconnect(bob_peer_id)).await.unwrap();

    let alice_stats = alice.send(GetConnectionStats).await.unwrap();
    let bob_stats = bob.send(GetConnectionStats).await.unwrap();

    assert_eq!(alice_stats.connected_peers, HashSet::from([]));
    assert_eq!(bob_stats.connected_peers, HashSet::from([]));
}

#[tokio::test]
async fn listen_address_is_reflected_in_stats() {
    let (_, _, alice, _, listen_address) = alice_and_bob([], []).await;

    let alice_stats = alice.send(GetConnectionStats).await.unwrap();

    assert_eq!(
        alice_stats.listen_addresses,
        HashSet::from([listen_address])
    );
}

#[tokio::test]
async fn cannot_open_substream_for_unhandled_protocol() {
    let (_, bob_peer_id, alice, _bob, _) = alice_and_bob([], []).await;

    let error = alice
        .send(OpenSubstream::single_protocol(
            bob_peer_id,
            "/foo/bar/1.0.0",
        ))
        .await
        .unwrap()
        .unwrap_err();

    assert!(matches!(
        error,
        libp2p_xtra::Error::NegotiationFailed(libp2p_xtra::NegotiationError::Failed)
    ))
}

#[tokio::test]
async fn cannot_connect_twice() {
    let (alice_peer_id, _bob_peer_id, _alice, bob, alice_listen) = alice_and_bob([], []).await;

    let error = bob
        .send(Connect(
            alice_listen.with(Protocol::P2p(alice_peer_id.into())),
        ))
        .await
        .unwrap()
        .unwrap_err();

    assert!(matches!(
        error,
        libp2p_xtra::Error::AlreadyConnected(twin) if twin == alice_peer_id
    ))
}

#[tokio::test]
async fn chooses_first_protocol_in_list_of_multiple() {
    let alice_hello_world_handler = HelloWorld::default().create(None).spawn_global();
    let (alice_peer_id, _, _alice, bob, _) = alice_and_bob(
        [(
            "/hello-world/1.0.0",
            alice_hello_world_handler.clone_channel(),
        )],
        [],
    )
    .await;

    let (actual_protocol, _) = bob
        .send(OpenSubstream::multiple_protocols(
            alice_peer_id,
            vec![
                "/hello-world/1.0.0",
                "/foo-bar/1.0.0", // This is unsupported by Alice.
            ],
        ))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(actual_protocol, "/hello-world/1.0.0");
}

#[cfg_attr(debug_assertions, tokio::test)] // The assertion for duplicate handlers only runs in debug mode.
#[should_panic(expected = "Duplicate handler declared for protocol /hello-world/1.0.0")]
async fn disallow_duplicate_handlers() {
    let hello_world_handler = HelloWorld::default().create(None).spawn_global();

    make_endpoint([
        ("/hello-world/1.0.0", hello_world_handler.clone_channel()),
        ("/hello-world/1.0.0", hello_world_handler.clone_channel()),
    ]);
}

#[tokio::test]
async fn falls_back_to_next_protocol_if_unsupported() {
    let alice_hello_world_handler = HelloWorld::default().create(None).spawn_global();
    let (alice_peer_id, _, _alice, bob, _) = alice_and_bob(
        [(
            "/hello-world/1.0.0",
            alice_hello_world_handler.clone_channel(),
        )],
        [],
    )
    .await;

    let (actual_protocol, _) = bob
        .send(OpenSubstream::multiple_protocols(
            alice_peer_id,
            vec![
                "/foo-bar/1.0.0", // This is unsupported by Alice.
                "/hello-world/1.0.0",
            ],
        ))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(actual_protocol, "/hello-world/1.0.0");
}

async fn alice_and_bob<const AN: usize, const BN: usize>(
    alice_inbound_substream_handlers: [(
        &'static str,
        Box<dyn StrongMessageChannel<NewInboundSubstream>>,
    ); AN],
    bob_inbound_substream_handlers: [(
        &'static str,
        Box<dyn StrongMessageChannel<NewInboundSubstream>>,
    ); BN],
) -> (
    PeerId,
    PeerId,
    Address<Endpoint>,
    Address<Endpoint>,
    Multiaddr,
) {
    let port = rand::random::<u16>();

    let (alice_peer_id, alice) = make_endpoint(alice_inbound_substream_handlers);
    let (bob_peer_id, bob) = make_endpoint(bob_inbound_substream_handlers);

    let alice_listen = format!("/memory/{port}").parse::<Multiaddr>().unwrap();

    alice.send(ListenOn(alice_listen.clone())).await.unwrap();

    bob.send(Connect(
        format!("/memory/{port}/p2p/{alice_peer_id}")
            .parse()
            .unwrap(),
    ))
    .await
    .unwrap()
    .unwrap();

    (alice_peer_id, bob_peer_id, alice, bob, alice_listen)
}

fn make_endpoint<const N: usize>(
    substream_handlers: [(
        &'static str,
        Box<dyn StrongMessageChannel<NewInboundSubstream>>,
    ); N],
) -> (PeerId, Address<Endpoint>) {
    let id = Keypair::generate_ed25519();
    let peer_id = id.public().to_peer_id();

    let endpoint = Endpoint::new(
        MemoryTransport::default(),
        id,
        Duration::from_secs(20),
        substream_handlers,
    )
    .create(None)
    .spawn_global();

    (peer_id, endpoint)
}

#[derive(Default)]
struct HelloWorld {
    tasks: Tasks,
}

#[xtra_productivity(message_impl = false)]
impl HelloWorld {
    async fn handle(&mut self, msg: NewInboundSubstream) {
        tracing::info!("New hello world stream from {}", msg.peer);

        self.tasks
            .add_fallible(hello_world_listener(msg.stream), move |e| async move {
                tracing::warn!("Hello world protocol with peer {} failed: {}", msg.peer, e);
            });
    }
}

#[async_trait]
impl Actor for HelloWorld {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

async fn hello_world_dialer(stream: libp2p_xtra::Substream, name: &'static str) -> Result<String> {
    let mut stream = asynchronous_codec::Framed::new(stream, asynchronous_codec::LengthCodec);

    stream.send(Bytes::from(name)).await?;
    let bytes = stream.next().await.context("Expected message")??;
    let message = String::from_utf8(bytes.to_vec())?;

    Ok(message)
}

async fn hello_world_listener(stream: libp2p_xtra::Substream) -> Result<()> {
    let mut stream =
        asynchronous_codec::Framed::new(stream, asynchronous_codec::LengthCodec).fuse();

    let bytes = stream.select_next_some().await?;
    let name = String::from_utf8(bytes.to_vec())?;

    stream.send(Bytes::from(format!("Hello {name}!"))).await?;

    Ok(())
}
