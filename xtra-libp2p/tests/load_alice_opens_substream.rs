use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use asynchronous_codec::Bytes;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::identity::Keypair;
use libp2p_core::transport::MemoryTransport;
use libp2p_core::Multiaddr;
use libp2p_core::PeerId;
use opentelemetry::sdk::trace::Config;
use opentelemetry::sdk::Resource;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use std::collections::HashSet;
use std::sync::Once;
use std::time::Duration;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor;
use xtra::Address;
use xtra_libp2p::endpoint::ConnectionDropped;
use xtra_libp2p::endpoint::ConnectionEstablished;
use xtra_libp2p::endpoint::Subscribers;
use xtra_libp2p::Connect;
use xtra_libp2p::Disconnect;
use xtra_libp2p::Endpoint;
use xtra_libp2p::ListenOn;
use xtra_libp2p::NewInboundSubstream;
use xtra_libp2p::OpenSubstream;
use xtra_productivity::xtra_productivity;
use xtras::SendAsyncSafe;

const BOB_EXPECTS_MESSAGE_WITHIN_SECS: u64 = 20;

#[tokio::test]
async fn give_10_bobs_then_passes() {
    test_runner(10).await
}

#[tokio::test]
async fn give_300_bobs_then_panics_because_some_bobs_get_starved() {
    test_runner(300).await
}

async fn test_runner(nr_of_bobs: usize) {
    init_tracing();

    const PROTOCOL: &str = "/foo/1.0.0";

    let port = rand::random::<u16>();

    let (alice, alice_endpoint, alice_peer_id) = create_alice(PROTOCOL.to_string()).await;

    let alice_listen = format!("/memory/{port}").parse::<Multiaddr>().unwrap();
    alice_endpoint
        .send(ListenOn(alice_listen.clone()))
        .await
        .unwrap();

    let mut bobs = Vec::new();

    for index in 0..nr_of_bobs {
        let (bob, bob_endpoint) = create_bob(format!("Bob-{index}"), PROTOCOL.to_string()).await;

        bob_endpoint
            .send(Connect(
                format!("/memory/{port}/p2p/{alice_peer_id}")
                    .parse()
                    .unwrap(),
            ))
            .await
            .unwrap()
            .unwrap();

        bobs.push((bob, bob_endpoint));
    }

    // This simulates piling up messages in Alice's protocol handler
    for (index, (bob, bob_endpoint)) in bobs.iter().enumerate() {
        // send a message to all the bobs
        alice.send(MessageAllBobs).await.unwrap();
        retry_until_message_received(bob.clone(), index + 1).await;
        bob_endpoint.send(Disconnect(alice_peer_id)).await.unwrap();
    }

    opentelemetry::global::shutdown_tracer_provider();
}

async fn retry_until_message_received(bob: Bob, expected: usize) {
    let future = {
        let address = bob.address.clone();
        async move {
            loop {
                let message_counter = address.send(GetReceivedMessageCount).await.unwrap();
                if message_counter == expected {
                    break;
                }

                #[allow(clippy::disallowed_methods)]
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };

    tokio::time::timeout(Duration::from_secs(BOB_EXPECTS_MESSAGE_WITHIN_SECS), future)
        .await
        .with_context(|| format!("{} never reached counter {}", bob.tag, expected))
        .unwrap()
}

async fn create_alice(
    protocol_name: String,
) -> (
    Address<AliceOpenSubstreamListener>,
    Address<Endpoint>,
    PeerId,
) {
    let (endpoint_addr, endpoint_context) = xtra::Context::new(None);

    let listener_actor = AliceOpenSubstreamListener::new(
        Box::leak(protocol_name.into_boxed_str()),
        endpoint_addr.clone(),
    )
    .create(None)
    .spawn_global();
    let id = Keypair::generate_ed25519();
    let peer_id = id.public().to_peer_id();

    let endpoint = Endpoint::new(
        Box::new(MemoryTransport::default),
        id,
        Duration::from_secs(20),
        [],
        Subscribers::new(
            vec![listener_actor.clone().into()],
            vec![listener_actor.clone().into()],
            vec![],
            vec![],
        ),
    );

    #[allow(clippy::disallowed_methods)]
    tokio::task::spawn(endpoint_context.run(endpoint));

    (listener_actor, endpoint_addr, peer_id)
}

#[derive(Clone, Debug)]
struct Bob {
    tag: String,
    address: Address<AliceOpensSubstreamDialer>,
}

async fn create_bob(tag: String, protocol_name: String) -> (Bob, Address<Endpoint>) {
    let (endpoint_addr, endpoint_context) = xtra::Context::new(None);

    let dialer_actor = AliceOpensSubstreamDialer::new(tag.clone(), protocol_name.clone())
        .create(None)
        .spawn_global();
    let id = Keypair::generate_ed25519();

    let endpoint = Endpoint::new(
        Box::new(MemoryTransport::default),
        id,
        Duration::from_secs(20),
        [(
            Box::leak(protocol_name.into_boxed_str()),
            dialer_actor.clone().into(),
        )],
        Subscribers::new(vec![], vec![], vec![], vec![]),
    );

    #[allow(clippy::disallowed_methods)]
    tokio::task::spawn(endpoint_context.run(endpoint));

    (
        Bob {
            tag,
            address: dialer_actor,
        },
        endpoint_addr,
    )
}

struct AliceOpenSubstreamListener {
    connected_bobs: HashSet<PeerId>,
    protocol: &'static str,
    endpoint: Address<Endpoint>,
}

impl AliceOpenSubstreamListener {
    pub fn new(protocol: &'static str, endpoint: Address<Endpoint>) -> Self {
        Self {
            connected_bobs: HashSet::default(),
            protocol,
            endpoint,
        }
    }
}

#[xtra_productivity(message_impl = false)]
impl AliceOpenSubstreamListener {
    async fn handle(&mut self, msg: ConnectionEstablished) {
        tracing::info!("Connection established with {}", msg.peer);

        self.connected_bobs.insert(msg.peer);
    }

    async fn handle(&mut self, msg: ConnectionDropped) {
        tracing::info!("Connection dropped for {}", msg.peer);

        self.connected_bobs.remove(&msg.peer);
    }
}

struct MessageAllBobs;

#[xtra_productivity]
impl AliceOpenSubstreamListener {
    async fn handle(&mut self, _msg: MessageAllBobs, ctx: &mut xtra::Context<Self>) {
        for peer in &self.connected_bobs {
            let future = {
                let peer = *peer;
                let endpoint = self.endpoint.clone();
                let protocol = self.protocol;
                async move {
                    let alice_to_bob = endpoint
                        .send(OpenSubstream::single_protocol(peer, protocol))
                        .await
                        .unwrap()
                        .unwrap();

                    alice_sends(alice_to_bob).await.unwrap();

                    tracing::info!("Finished {protocol} for peer {peer}");
                    anyhow::Ok(())
                }
            };

            let this = ctx.address().expect("self to be alive");
            tokio_extras::spawn_fallible(&this, future, move |e| async move {
                tracing::error!("Listener failed to spawn future: {e:#}");
            });
        }
    }
}

#[async_trait]
impl Actor for AliceOpenSubstreamListener {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

struct AliceOpensSubstreamDialer {
    tag: String,
    protocol: String,
    received_messages_counter: usize,
}

impl AliceOpensSubstreamDialer {
    pub fn new(tag: String, protocol: String) -> Self {
        Self {
            tag,
            protocol,
            received_messages_counter: 0,
        }
    }
}

#[xtra_productivity(message_impl = false)]
impl AliceOpensSubstreamDialer {
    async fn handle(&mut self, msg: NewInboundSubstream, ctx: &mut xtra::Context<Self>) {
        tracing::info!("{} handling NewInboundSubstream", self.protocol);

        let this = ctx.address().expect("self to be alive");

        let protocol = self.protocol.clone();
        let future = {
            let this = this.clone();
            let tag = self.tag.clone();
            async move {
                bob_receives(msg.stream).await?;
                this.send_async_safe(Done).await?;

                tracing::info!("{tag} sent ListenerDone for protocol {protocol}");

                anyhow::Ok(())
            }
        };

        tokio_extras::spawn_fallible(&this, future, move |e| async move {
            tracing::warn!("Parallel message with peer {} failed: {}", msg.peer, e);
        });
    }
}

#[xtra_productivity]
impl AliceOpensSubstreamDialer {
    async fn handle(&mut self, _: Done) {
        self.received_messages_counter += 1;

        tracing::info!(
            "{} received ListenerDone for trigger time {}",
            self.protocol,
            self.received_messages_counter
        );
    }

    async fn handle(&mut self, _check_done: GetReceivedMessageCount) -> usize {
        self.received_messages_counter
    }
}

#[async_trait]
impl Actor for AliceOpensSubstreamDialer {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

struct Done;
struct GetReceivedMessageCount;

pub fn into_arr<T, const N: usize>(v: Vec<T>) -> [T; N] {
    v.try_into()
        .unwrap_or_else(|v: Vec<T>| panic!("Expected a Vec of length {} but it was {}", N, v.len()))
}

async fn alice_sends(stream: xtra_libp2p::Substream) -> Result<()> {
    let mut stream =
        asynchronous_codec::Framed::new(stream, asynchronous_codec::LengthCodec).fuse();

    stream.send(Bytes::from("Message")).await?;

    Ok(())
}

async fn bob_receives(stream: xtra_libp2p::Substream) -> Result<()> {
    let mut stream =
        asynchronous_codec::Framed::new(stream, asynchronous_codec::LengthCodec).fuse();

    let bytes = stream.select_next_some().await?;
    let name = String::from_utf8(bytes.to_vec())?;

    assert_eq!(name, "Message");

    Ok(())
}

static INIT_OTLP_EXPORTER: Once = Once::new();

pub fn init_tracing() {
    INIT_OTLP_EXPORTER.call_once(|| {
        let cfg = Config::default().with_resource(Resource::new([KeyValue::new(
            "service.name",
            "daemon-tests",
        )]));

        let tracer = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_trace_config(cfg)
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .grpcio()
                    .with_endpoint("localhost:4317"),
            )
            .install_simple()
            .unwrap();

        let filter = EnvFilter::from_default_env()
            // apply warning level globally
            .add_directive(LevelFilter::WARN.into())
            // log traces from test itself
            .add_directive("regression=debug".parse().unwrap())
            .add_directive("xtra=debug".parse().unwrap())
            .add_directive("xtra_libp2p=debug".parse().unwrap())
            .add_directive("xtras=debug".parse().unwrap());

        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_test_writer()
            .with_filter(filter);
        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

        Registry::default().with(telemetry).with(fmt_layer).init();
    })
}
