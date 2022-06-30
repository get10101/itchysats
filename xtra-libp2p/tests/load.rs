use crate::util::make_node;
use anyhow::Context as _;
use anyhow::Result;
use async_trait::async_trait;
use asynchronous_codec::Bytes;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::Multiaddr;
use rand::distributions::Distribution;
use rand::distributions::Uniform;
use rand::prelude::StdRng;
use rand::SeedableRng;
use std::time::Duration;
use tokio_tasks::Tasks;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::util::SubscriberInitExt;
use xtra::message_channel::StrongMessageChannel;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor;
use xtra::Address;
use xtra_libp2p::Connect;
use xtra_libp2p::ListenOn;
use xtra_libp2p::NewInboundSubstream;
use xtra_libp2p::OpenSubstream;
use xtra_productivity::xtra_productivity;
use xtras::SendAsyncSafe;

mod util;

struct SomeMessageExchange {
    protocol: String,
    done_times_count: usize,
    tasks: Tasks,
}

impl SomeMessageExchange {
    pub fn new(protocol: String) -> Self {
        Self {
            protocol,
            done_times_count: 0,
            tasks: Default::default(),
        }
    }
}

#[xtra_productivity(message_impl = false)]
impl SomeMessageExchange {
    async fn handle(&mut self, msg: NewInboundSubstream, ctx: &mut xtra::Context<Self>) {
        tracing::info!("{} handling NewInboundSubstream", self.protocol);

        let this = ctx.address().expect("self to be alive");

        let protocol = self.protocol.clone();
        let future = async move {
            some_message_exchange_listener(msg.stream).await?;
            this.send_async_safe(ListenerDone).await?;

            tracing::info!("{} sent ListenerDone", protocol);

            anyhow::Ok(())
        };

        self.tasks.add_fallible(future, move |e| async move {
            tracing::warn!("Parallel message with peer {} failed: {}", msg.peer, e);
        });
    }
}

#[xtra_productivity]
impl SomeMessageExchange {
    async fn handle(&mut self, _: ListenerDone) {
        self.done_times_count += 1;

        tracing::info!(
            "{} received ListenerDone for trigger time {}",
            self.protocol,
            self.done_times_count
        );
    }

    async fn handle(&mut self, check_done: CheckListenerDone) -> bool {
        self.done_times_count == check_done.0
    }
}

struct ListenerDone;
struct CheckListenerDone(usize);

#[async_trait]
impl Actor for SomeMessageExchange {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

pub fn into_arr<T, const N: usize>(v: Vec<T>) -> [T; N] {
    v.try_into()
        .unwrap_or_else(|v: Vec<T>| panic!("Expected a Vec of length {} but it was {}", N, v.len()))
}

// TODO: If we go over 100 then *sometimes* we fail by the test just "hanging" in the end, i.e. we
// don't finish properly
const BOBS: usize = 10;

// TODO: It does not really matter how often we trigger, if we run with 200 BOBS I see consistent
// failure for multiple_bobs_one_protocol_load_test because something "hangs"
const TRIGGER_TIMES: usize = 1_000;

// TODO: Sometimes it takes longer and we fail to establish a connection
const BOB_WAIT_BUFFER_MILLIS_LOWER_BOUND: u64 = 800;
const BOB_WAIT_BUFFER_MILLIS_UPPER_BOUND: u64 = 900;

const BOB_WAIT_BETWEEN_TRIGGER_MILLIS_LOWER_BOUND: u64 = 400;
const BOB_WAIT_BETWEEN_TRIGGER_MILLIS_UPPER_BOUND: u64 = 600;

#[tokio::test(flavor = "multi_thread", worker_threads = 1000)]
async fn multiple_bobs_one_protocol_load_test() {
    const SOME_PROTOCOL_NAME: &str = "/some-protocol/1.0.0";

    let _guard = init_tracing();

    let alice_pme_handler = SomeMessageExchange::new(SOME_PROTOCOL_NAME.to_string())
        .create(None)
        .spawn_global();

    let port = rand::random::<u16>();

    let alice = make_node([(
        SOME_PROTOCOL_NAME,
        xtra::message_channel::StrongMessageChannel::clone_channel(&alice_pme_handler),
    )]);
    let alice_listen = format!("/memory/{port}").parse::<Multiaddr>().unwrap();
    alice
        .endpoint
        .send(ListenOn(alice_listen.clone()))
        .await
        .unwrap();
    let alice_peer_id = &alice.peer_id;

    // Give Alice some time to start up and listen
    tokio::time::sleep(Duration::from_secs(1)).await;

    for _bob in 0..BOBS {
        let alice_peer_id = *alice_peer_id;
        let dialer = async move {
            let bob = make_node([]);
            bob.endpoint
                .send(Connect(
                    format!("/memory/{port}/p2p/{alice_peer_id}")
                        .parse()
                        .unwrap(),
                ))
                .await
                .unwrap()
                .unwrap();

            let mut rng: StdRng = SeedableRng::from_entropy();
            let rand_millis = Uniform::from(
                BOB_WAIT_BUFFER_MILLIS_LOWER_BOUND..BOB_WAIT_BUFFER_MILLIS_UPPER_BOUND,
            );
            let sleep_millis = rand_millis.sample(&mut rng);

            tracing::info!("Sleeping for {} millis", sleep_millis);

            // Give Bob some time to connect
            tokio::time::sleep(Duration::from_millis(sleep_millis)).await;

            for _ in 0..TRIGGER_TIMES {
                let bob_to_alice = bob
                    .endpoint
                    .send(OpenSubstream::single_protocol(
                        alice.peer_id,
                        SOME_PROTOCOL_NAME,
                    ))
                    .await
                    .unwrap()
                    .unwrap();

                some_message_exchange_dialer(bob_to_alice).await.unwrap();

                if TRIGGER_TIMES > 1 {
                    let mut rng: StdRng = SeedableRng::from_entropy();
                    let rand_millis = Uniform::from(
                        BOB_WAIT_BETWEEN_TRIGGER_MILLIS_LOWER_BOUND
                            ..BOB_WAIT_BETWEEN_TRIGGER_MILLIS_UPPER_BOUND,
                    );
                    let sleep_millis = rand_millis.sample(&mut rng);
                    tokio::time::sleep(Duration::from_millis(sleep_millis)).await;
                }
            }
        };

        #[allow(clippy::disallowed_methods)]
        tokio::task::spawn(dialer);
    }

    let ensure_done = async move {
        loop {
            let done = &alice_pme_handler
                .send(CheckListenerDone(TRIGGER_TIMES * BOBS))
                .await
                .unwrap();

            if *done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };

    tokio::time::timeout(Duration::from_secs(10), ensure_done)
        .await
        .unwrap();
}

#[tokio::test]
async fn multiple_bobs_multiple_distinct_protocols_load_test() {
    let _guard = init_tracing();

    let mut alice_inbound_substream_handlers: Vec<(
        &'static str,
        Box<dyn StrongMessageChannel<NewInboundSubstream>>,
    )> = Vec::new();
    let mut protocols: Vec<String> = Vec::new();
    let mut alice_pme_actors: Vec<Address<SomeMessageExchange>> = Vec::new();

    // create one protocol for each Bob (including specific handler for alice for that protocol)
    for index in 0..BOBS {
        let protocol = format!("/some-protocol-{}/1.0.0", index);

        let pme_handler = SomeMessageExchange::new(protocol.to_string())
            .create(None)
            .spawn_global();

        alice_inbound_substream_handlers.push((
            Box::leak(protocol.clone().into_boxed_str()),
            xtra::message_channel::StrongMessageChannel::clone_channel(&pme_handler),
        ));
        alice_pme_actors.push(pme_handler);
        protocols.push(protocol);
    }

    let alice_inbound_substream_handlers = into_arr::<
        (
            &'static str,
            Box<dyn StrongMessageChannel<NewInboundSubstream>>,
        ),
        BOBS,
    >(alice_inbound_substream_handlers);

    let port = rand::random::<u16>();

    let alice = make_node(alice_inbound_substream_handlers);
    let alice_listen = format!("/memory/{port}").parse::<Multiaddr>().unwrap();
    alice
        .endpoint
        .send(ListenOn(alice_listen.clone()))
        .await
        .unwrap();
    let alice_peer_id = &alice.peer_id;

    // Give Alice some time to start up and listen
    tokio::time::sleep(Duration::from_secs(1)).await;

    for protocol in protocols {
        let alice_peer_id = *alice_peer_id;
        let dialer = async move {
            let bob = make_node([]);
            bob.endpoint
                .send(Connect(
                    format!("/memory/{port}/p2p/{alice_peer_id}")
                        .parse()
                        .unwrap(),
                ))
                .await
                .unwrap()
                .unwrap();

            let mut rng: StdRng = SeedableRng::from_entropy();
            let rand_millis = Uniform::from(
                BOB_WAIT_BUFFER_MILLIS_LOWER_BOUND..BOB_WAIT_BUFFER_MILLIS_UPPER_BOUND,
            );
            let sleep_millis = rand_millis.sample(&mut rng);

            tracing::info!("Sleeping for {} millis", sleep_millis);

            // Give Bob some time to connect
            tokio::time::sleep(Duration::from_millis(sleep_millis)).await;

            for _ in 0..TRIGGER_TIMES {
                let bob_to_alice = bob
                    .endpoint
                    .send(OpenSubstream::single_protocol(
                        alice.peer_id,
                        Box::leak(protocol.clone().into_boxed_str()),
                    ))
                    .await
                    .unwrap()
                    .unwrap();

                some_message_exchange_dialer(bob_to_alice).await.unwrap();

                if TRIGGER_TIMES > 1 {
                    let mut rng: StdRng = SeedableRng::from_entropy();
                    let rand_millis = Uniform::from(
                        BOB_WAIT_BETWEEN_TRIGGER_MILLIS_LOWER_BOUND
                            ..BOB_WAIT_BETWEEN_TRIGGER_MILLIS_UPPER_BOUND,
                    );
                    let sleep_millis = rand_millis.sample(&mut rng);
                    tokio::time::sleep(Duration::from_millis(sleep_millis)).await;
                }
            }
        };

        #[allow(clippy::disallowed_methods)]
        tokio::task::spawn(dialer);
    }

    let ensure_done = async move {
        let mut done = false;
        while !done {
            for pme_handler in &alice_pme_actors {
                done = pme_handler
                    .send(CheckListenerDone(TRIGGER_TIMES))
                    .await
                    .unwrap();
            }

            if done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };

    tokio::time::timeout(Duration::from_secs(10), ensure_done)
        .await
        .unwrap();
}

async fn some_message_exchange_dialer(stream: xtra_libp2p::Substream) -> Result<()> {
    let mut stream =
        asynchronous_codec::Framed::new(stream, asynchronous_codec::LengthCodec).fuse();

    tracing::info!("dialer before send 1");
    stream.send(Bytes::from("Dialer1")).await?;
    tracing::info!("dialer after send1");

    tracing::info!("dialer before receive 1");
    let bytes = stream
        .select_next_some()
        .await
        .context("Expected message")?;
    tracing::info!("dialer after receive 1");
    let message = String::from_utf8(bytes.to_vec())?;

    assert_eq!(message, "Listener1");
    // tokio::time::sleep(Duration::from_millis(10)).await;

    tracing::info!("dialer before send 2");
    stream.send(Bytes::from("Dialer2")).await?;
    tracing::info!("dialer after send 2");

    tracing::info!("dialer before receive 2");
    let bytes = stream
        .select_next_some()
        .await
        .context("Expected message")?;
    tracing::info!("dialer after receive 2");
    let message = String::from_utf8(bytes.to_vec())?;

    assert_eq!(message, "Listener2");

    Ok(())
}

async fn some_message_exchange_listener(stream: xtra_libp2p::Substream) -> Result<()> {
    let mut stream =
        asynchronous_codec::Framed::new(stream, asynchronous_codec::LengthCodec).fuse();

    tracing::info!("listener before receive 1");
    let bytes = stream.select_next_some().await?;
    tracing::info!("listener after receive 1");
    let name = String::from_utf8(bytes.to_vec())?;

    tracing::info!("listener before send 1");
    stream.send(Bytes::from("Listener1")).await?;
    tracing::info!("listener after send 1");

    // tokio::time::sleep(Duration::from_millis(20)).await;

    assert_eq!(name, "Dialer1");

    // tokio::time::sleep(Duration::from_millis(10)).await;

    tracing::info!("listener before receive 2");
    let bytes = stream.select_next_some().await?;
    tracing::info!("listener after receive 2");
    let name = String::from_utf8(bytes.to_vec())?;

    tracing::info!("listener before send 2");
    stream.send(Bytes::from("Listener2")).await?;
    tracing::info!("listener after send 2");

    assert_eq!(name, "Dialer2");

    Ok(())
}

pub fn init_tracing() -> DefaultGuard {
    tracing_subscriber::fmt()
        .with_env_filter("WARN")
        .with_env_filter("load=debug")
        .with_env_filter("xtra=debug")
        .with_env_filter("xtra_libp2p=debug")
        .with_env_filter("xtras=debug")
        .with_test_writer()
        .set_default()
}
