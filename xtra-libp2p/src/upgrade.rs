use crate::verify_peer_id::VerifyPeerId;
use crate::Connection;
use futures::channel::mpsc;
use futures::future;
use futures::AsyncRead;
use futures::AsyncWrite;
use futures::FutureExt;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::identity::Keypair;
use libp2p_core::transport::timeout::TransportTimeout;
use libp2p_core::transport::Boxed;
use libp2p_core::upgrade;
use libp2p_core::upgrade::Version;
use libp2p_core::Endpoint;
use libp2p_core::Transport;
use libp2p_noise as noise;
use multistream_select::NegotiationError;
use std::time::Duration;
use void::Void;
use yamux::Mode;

/// Upgrades the given [`Transport`].
///
/// We apply:
/// - Noise encryption and authentication
/// - PeerID verification for each connection
/// - Yamux multiplexing
/// - Connection upgrade timeout
pub fn transport<T>(
    transport: T,
    identity: &Keypair,
    supported_inbound_protocols: Vec<&'static str>,
    connection_timeout: Duration,
) -> Boxed<Connection>
where
    T: Transport + Clone + Send + Sync + 'static,
    T::Output: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    T::Error: Send + Sync,
    T::Listener: Send + 'static,
    T::Dial: Send + 'static,
    T::ListenerUpgrade: Send + 'static,
{
    let identity = noise::Keypair::<noise::X25519Spec>::new()
        .into_authentic(identity)
        .expect("ed25519 signing does not fail");

    let authenticated = transport.and_then(|conn, endpoint| {
        upgrade::apply(
            conn,
            noise::NoiseConfig::xx(identity).into_authenticated(),
            endpoint,
            Version::V1,
        )
    });

    let peer_id_verified = VerifyPeerId::new(authenticated);

    let multiplexed = peer_id_verified.and_then(|(peer_id, conn), endpoint| {
        upgrade::apply(
            conn,
            upgrade::from_fn::<_, _, _, _, _, Void>(
                b"/yamux/1.0.0",
                move |conn, endpoint| async move {
                    Ok(match endpoint {
                        Endpoint::Dialer => (
                            peer_id,
                            yamux::Connection::new(conn, yamux::Config::default(), Mode::Client),
                        ),
                        Endpoint::Listener => (
                            peer_id,
                            yamux::Connection::new(conn, yamux::Config::default(), Mode::Server),
                        ),
                    })
                },
            ),
            endpoint,
            Version::V1,
        )
    });

    let protocols_negotiated = multiplexed.map(move |(peer, mut connection), _| {
        let control = connection.control();

        let (mut sender, receiver) = mpsc::channel(5); // Use a bounded channel to allow caller to exercise back-pressure.

        let worker = async move {
            while let Ok(Some(stream)) = connection.next_stream().await {
                match future::poll_fn(|cx| sender.poll_ready(cx)).await {
                    Ok(()) => {}
                    Err(e) => {
                        if e.is_disconnected() {
                            break; // Consumer is no longer interested
                        }

                        debug_assert!(
                            !e.is_full(),
                            "We checked that there is space in the queue with `poll_ready`"
                        )
                    }
                }

                match sender.send(stream).await {
                    Ok(()) => {}
                    Err(e) => {
                        if e.is_disconnected() {
                            break; // Consumer is no longer interested
                        }

                        debug_assert!(
                            !e.is_full(),
                            "We checked that there is space in the queue with `poll_ready`"
                        )
                    }
                }
            }
        }
        .boxed();

        let incoming = receiver
            .then(move |stream| {
                let supported_protocols = supported_inbound_protocols.clone();

                async move {
                    let result = tokio::time::timeout(
                        connection_timeout,
                        multistream_select::listener_select_proto(stream, &supported_protocols),
                    )
                    .await;

                    match result {
                        Ok(Ok((protocol, stream))) => Ok(Ok((stream, *protocol))),
                        Ok(Err(e)) => Ok(Err(Error::NegotiationFailed(e))),
                        Err(_timeout) => Ok(Err(Error::NegotiationTimeoutReached)),
                    }
                }
            })
            .boxed();

        (peer, control, incoming, worker)
    });

    TransportTimeout::new(protocols_negotiated, connection_timeout).boxed()
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Timeout in protocol negotiation")]
    NegotiationTimeoutReached,
    #[error("Failed to negotiate protocol")]
    NegotiationFailed(#[from] NegotiationError),
}
