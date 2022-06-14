use anyhow::Result;
use async_trait::async_trait;
use asynchronous_codec::Bytes;
use clap::Parser;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::identity::Keypair;
use libp2p_core::Multiaddr;
use libp2p_tcp::TokioTcpConfig;
use std::time::Duration;
use tokio::time::sleep;
use tokio_tasks::Tasks;
use tracing::Level;
use xtra::prelude::*;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra_libp2p::endpoint::Subscribers;
use xtra_libp2p::listener;
use xtra_libp2p::Endpoint;
use xtra_libp2p::NewInboundSubstream;
use xtra_productivity::xtra_productivity;
use xtras::supervisor;

// Listen on TCP

#[derive(Parser)]
struct Opts {
    #[clap(long, default_value = "10000")]
    port: u16,

    #[clap(long, default_value = "120")]
    duration_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let opts = Opts::parse();

    tracing_subscriber::fmt()
        .with_max_level(Level::TRACE)
        .init();

    let id = Keypair::generate_ed25519();

    let peer_id = id.public().to_peer_id();
    let port = opts.port;

    let multiaddr_str = format!("/ip4/127.0.0.1/tcp/{port}/p2p/{peer_id}");
    tracing::info!("This listener will be active for {}s", opts.duration_secs);
    tracing::info!("Connect to this listener by using the multiaddr: {multiaddr_str}. e.g:");
    tracing::info!("cargo run --example hello_world_dialer -- --multiaddr {multiaddr_str}");

    let hello_world_addr = HelloWorld::default().create(None).spawn_global();

    let endpoint_addr = Endpoint::new(
        Box::new(TokioTcpConfig::new),
        id,
        Duration::from_secs(30),
        [(
            "/hello-world/1.0.0",
            xtra::message_channel::StrongMessageChannel::clone_channel(&hello_world_addr),
        )],
        Subscribers::default(),
    )
    .create(None)
    .spawn_global();

    let endpoint_listen = multiaddr_str.parse::<Multiaddr>().unwrap();

    let listener_constructor = move || {
        let endpoint_listen = endpoint_listen.clone();
        let endpoint_addr = endpoint_addr.clone();
        listener::Actor::new(endpoint_addr, endpoint_listen)
    };
    let (supervisor, _listener_actor) = supervisor::Actor::with_policy(
        listener_constructor,
        |_: &listener::Error| true, // always restart listener actor
    );
    let _listener_supervisor = supervisor.create(None).spawn_global();

    sleep(Duration::from_secs(opts.duration_secs)).await;

    Ok(())
}

#[derive(Default)]
pub struct HelloWorld {
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

pub async fn hello_world_listener(stream: xtra_libp2p::Substream) -> Result<()> {
    let mut stream =
        asynchronous_codec::Framed::new(stream, asynchronous_codec::LengthCodec).fuse();

    let bytes = stream.select_next_some().await?;
    let name = String::from_utf8(bytes.to_vec())?;

    stream.send(Bytes::from(format!("Hello {name}!"))).await?;

    Ok(())
}
