use crate::{noise, send_to_socket, taker_cfd, wire};
use anyhow::Result;
use futures::{Stream, StreamExt};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::codec::FramedRead;
use xtra::prelude::MessageChannel;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor as _;

const CONNECTION_RETRY_INTERVAL: Duration = Duration::from_secs(5);

pub struct Actor {
    pub send_to_maker: Box<dyn MessageChannel<wire::TakerToMaker>>,
    pub read_from_maker: Box<dyn Stream<Item = taker_cfd::MakerStreamMessage> + Unpin + Send>,
}

impl Actor {
    pub async fn new(
        maker_addr: SocketAddr,
        maker_noise_static_pk: x25519_dalek::PublicKey,
        noise_static_sk: x25519_dalek::StaticSecret,
    ) -> Result<Self> {
        let (read, write, noise) = loop {
            let socket = tokio::net::TcpSocket::new_v4().expect("Be able ta create a socket");
            if let Ok(mut connection) = socket.connect(maker_addr).await {
                let noise = noise::initiator_handshake(
                    &mut connection,
                    &noise_static_sk,
                    &maker_noise_static_pk,
                )
                .await?;
                let (read, write) = connection.into_split();
                break (read, write, Arc::new(Mutex::new(noise)));
            } else {
                tracing::warn!(
                    "Could not connect to the maker, retrying in {}s ...",
                    CONNECTION_RETRY_INTERVAL.as_secs()
                );
                tokio::time::sleep(CONNECTION_RETRY_INTERVAL).await;
            }
        };

        let send_to_maker = send_to_socket::Actor::new(write, noise.clone())
            .create(None)
            .spawn_global();

        let read = FramedRead::new(read, wire::EncryptedJsonCodec::new(noise))
            .map(move |item| taker_cfd::MakerStreamMessage { item });

        Ok(Self {
            send_to_maker: Box::new(send_to_maker),
            read_from_maker: Box::new(read),
        })
    }
}
