use anyhow::Context;
use anyhow::Result;
use asynchronous_codec::Bytes;
use clap::Parser;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::identity::Keypair;
use libp2p_core::Multiaddr;
use libp2p_core::PeerId;
use libp2p_tcp::TokioTcpConfig;
use libp2p_xtra::dialer;
use libp2p_xtra::Endpoint;
use libp2p_xtra::OpenSubstream;
use std::time::Duration;
use tokio::time::sleep;
use xtra::prelude::*;
use xtra::spawn::TokioGlobalSpawnExt;
use xtras::supervisor;

#[derive(Parser)]
struct Opts {
    #[clap(long)]
    multiaddr: Multiaddr,

    #[clap(long, default_value = "ExampleDialer")]
    name: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("debug").init();

    let opts = Opts::parse();

    let id = Keypair::generate_ed25519();

    let endpoint_addr = Endpoint::new(TokioTcpConfig::new(), id, Duration::from_secs(20), [])
        .create(None)
        .spawn_global();

    let dialer_constructor = {
        let connect_addr = opts.multiaddr.clone();
        let endpoint_addr = endpoint_addr.clone();
        move || dialer::Actor::new(endpoint_addr.clone(), connect_addr.clone())
    };

    let (supervisor, _dialer_actor) = supervisor::Actor::with_policy(
        dialer_constructor,
        |_: &dialer::Error| true, // always restart dialer actor
    );
    let _dialer_supervisor = supervisor.create(None).spawn_global();

    sleep(Duration::from_secs(1)).await;

    let stream = endpoint_addr
        .send(OpenSubstream::single_protocol(
            PeerId::try_from_multiaddr(&opts.multiaddr).unwrap(),
            "/hello-world/1.0.0",
        ))
        .await
        .unwrap()
        .unwrap();

    let message = hello_world_dialer(stream, opts.name).await.unwrap();

    tracing::info!("{message}");

    Ok(())
}

async fn hello_world_dialer(stream: libp2p_xtra::Substream, name: String) -> Result<String> {
    let mut stream = asynchronous_codec::Framed::new(stream, asynchronous_codec::LengthCodec);

    stream.send(Bytes::from(name)).await?;
    let bytes = stream.next().await.context("Expected message")??;
    let message = String::from_utf8(bytes.to_vec())?;

    Ok(message)
}
