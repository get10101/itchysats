use daemon::{send_to_socket, taker_cfd, wire};
use futures::{Stream, StreamExt};
use std::net::SocketAddr;
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
    pub async fn new(maker: SocketAddr) -> Self {
        let (read, write) = loop {
            let socket = tokio::net::TcpSocket::new_v4().expect("Be able ta create a socket");
            if let Ok(connection) = socket.connect(maker).await {
                break connection.into_split();
            } else {
                tracing::warn!(
                    "Could not connect to the maker, retrying in {}s ...",
                    CONNECTION_RETRY_INTERVAL.as_secs()
                );
                tokio::time::sleep(CONNECTION_RETRY_INTERVAL).await;
            }
        };

        let send_to_maker = send_to_socket::Actor::new(write)
            .create(None)
            .spawn_global();

        let read = FramedRead::new(read, wire::JsonCodec::default())
            .map(move |item| taker_cfd::MakerStreamMessage { item });

        Self {
            send_to_maker: Box::new(send_to_maker),
            read_from_maker: Box::new(read),
        }
    }
}
