use crate::util::make_node;
use anyhow::bail;
use async_trait::async_trait;
use libp2p_core::multiaddr::Protocol;
use libp2p_core::Multiaddr;
use opentelemetry_otlp::WithExportConfig;
use std::time::Duration;
use time::macros::format_description;
use tokio_extras::Tasks;
use tracing_subscriber::fmt::time::UtcTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use xtra::spawn::Spawner;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor;
use xtra_libp2p::Connect;
use xtra_libp2p::ListenOn;
use xtra_libp2p::NewInboundSubstream;
use xtra_libp2p::OpenSubstream;
use xtra_productivity::xtra_productivity;
use xtras::SendAsyncSafe;

mod util;

const N_BOBS: usize = 1;

const ALICE_PORT: u64 = 1_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 100)]
async fn many_bobs_dialing_one_alice() {
    init_tracing();

    let alice_listener = ListenerActor::new().create(None).spawn_global();

    let alice = make_node([(
        protocol::NAME,
        xtra::message_channel::MessageChannel::new(alice_listener.clone()),
    )]);

    alice
        .endpoint
        .send(ListenOn(
            Multiaddr::empty().with(Protocol::Memory(ALICE_PORT)),
        ))
        .await
        .unwrap();

    // Give Alice some time to start up and listen
    tokio_extras::time::sleep(Duration::from_secs(1)).await;

    let mut tasks = Tasks::default();
    tasks.spawn(async move {
        for i in 0..50000 {
            tracing::debug!(%i, "Spawning lazy bob");

            let bob = make_node([]);
            bob.endpoint
                .send(Connect(
                    Multiaddr::empty()
                        .with(Protocol::Memory(ALICE_PORT))
                        .with(Protocol::P2p(alice.peer_id.into())),
                ))
                .await
                .unwrap()
                .unwrap();

            std::mem::forget(bob);
        }
    });

    for _bob in 0..N_BOBS {
        let bob = make_node([]);
        bob.endpoint
            .send(Connect(
                Multiaddr::empty()
                    .with(Protocol::Memory(ALICE_PORT))
                    .with(Protocol::P2p(alice.peer_id.into())),
            ))
            .await
            .unwrap()
            .unwrap();

        for _attempt in 0..100 {
            let bob = bob.clone();
            let dialer = async move {
                let now = std::time::Instant::now();
                let stream = loop {
                    match bob
                        .endpoint
                        .send(OpenSubstream::single_protocol(
                            alice.peer_id,
                            protocol::NAME,
                        ))
                        .await?
                    {
                        Ok(stream) => break anyhow::Ok(stream),
                        Err(xtra_libp2p::Error::NoConnection(_)) => {
                            tokio_extras::time::sleep(Duration::from_millis(100)).await;
                        }
                        Err(e) => bail!(e),
                    }
                }?;
                tracing::info!("Substream: {}ms", now.elapsed().as_millis());

                let now = std::time::Instant::now();
                protocol::dialer(stream).await?;
                tracing::info!("Took {}ms to dial", now.elapsed().as_millis());

                anyhow::Ok(())
            };

            tasks.add_fallible(dialer, move |e| async move {
                tracing::error!(dialer=%bob.peer_id, "Dialer failed: {}", e)
            });
        }
    }

    let ensure_done = async move {
        loop {
            let done = &alice_listener
                .send(CheckListenerDone(N_BOBS * 100))
                .await
                .unwrap();

            if *done {
                break;
            }
            tokio_extras::time::sleep(Duration::from_millis(10)).await;
        }
    };

    tokio::time::timeout(Duration::from_secs(120), ensure_done)
        .await
        .unwrap();
}

struct ListenerActor {
    done_times_count: usize,
    tasks: Tasks,
}

impl ListenerActor {
    pub fn new() -> Self {
        Self {
            done_times_count: 0,
            tasks: Default::default(),
        }
    }
}

#[xtra_productivity(message_impl = false)]
impl ListenerActor {
    async fn handle(&mut self, msg: NewInboundSubstream, ctx: &mut xtra::Context<Self>) {
        tracing::debug!(dialer = %msg.peer, "Handling NewInboundSubstream");

        let this = ctx.address().expect("self to be alive");
        let future = async move {
            protocol::listener(msg.stream).await?;

            this.send_async_safe(ListenerDone).await?;

            anyhow::Ok(())
        };

        self.tasks.add_fallible(future, move |e| async move {
            tracing::error!(dialer = %msg.peer, "Listener failed: {}", e);
        });
    }
}

#[xtra_productivity]
impl ListenerActor {
    async fn handle(&mut self, _: ListenerDone) {
        self.done_times_count += 1;

        tracing::debug!(count = %self.done_times_count, "Listener done");
    }

    async fn handle(&mut self, check_done: CheckListenerDone) -> bool {
        self.done_times_count == check_done.0
    }
}

struct ListenerDone;
struct CheckListenerDone(usize);

mod protocol {
    use anyhow::Context;
    use anyhow::Result;
    use asynchronous_codec::Bytes;
    use asynchronous_codec::FramedRead;
    use asynchronous_codec::FramedWrite;
    use asynchronous_codec::LengthCodec;
    use futures::SinkExt;
    use futures::StreamExt;

    pub const NAME: &str = "/foo/1.0.0";

    pub async fn dialer(stream: xtra_libp2p::Substream) -> Result<()> {
        let mut stream = FramedWrite::new(stream, LengthCodec);

        stream.send(Bytes::from("foo")).await?;

        Ok(())
    }

    pub async fn listener(stream: xtra_libp2p::Substream) -> Result<()> {
        let mut stream = FramedRead::new(stream, LengthCodec);

        let now = std::time::Instant::now();
        let bytes = stream.next().await.context("empty stream")??;
        tracing::info!("Waited {}ms for message", now.elapsed().as_millis());

        let name = String::from_utf8(bytes.to_vec())?;
        assert_eq!(name, "foo");

        Ok(())
    }
}

fn init_tracing() {
    opentelemetry::global::set_text_map_propagator(
        opentelemetry::sdk::propagation::TraceContextPropagator::new(),
    );

    opentelemetry::global::set_error_handler(|error| {
            ::tracing::error!(target: "opentelemetry", "OpenTelemetry error occurred: {:#}", anyhow::anyhow!(error));
        })
        .expect("to be able to set error handler");

    let cfg = opentelemetry::sdk::trace::Config::default().with_resource(
        opentelemetry::sdk::Resource::new([opentelemetry::KeyValue::new(
            "service.name",
            "load_test",
        )]),
    );

    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_trace_config(cfg)
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint("http://localhost:4317"),
        )
        .install_batch(opentelemetry::runtime::Tokio)
        .unwrap();

    let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

    let console_layer = console_subscriber::spawn();

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(true);

    let fmt_layer = fmt_layer
        .compact()
        .with_timer(UtcTime::new(format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second]"
        )))
        .boxed();

    let filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive(tracing::metadata::LevelFilter::INFO.into())
        .add_directive("load=debug".parse().unwrap())
        .add_directive("xtra_libp2p=debug".parse().unwrap())
        .add_directive("tokio=trace".parse().unwrap())
        .add_directive("runtime=trace".parse().unwrap());

    let fmt_layer = fmt_layer.with_filter(filter);

    Registry::default()
        .with(telemetry)
        .with(console_layer)
        .with(fmt_layer)
        .try_init()
        .unwrap();

    tracing::info!("Initialized logger");
}

#[async_trait]
impl Actor for ListenerActor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
