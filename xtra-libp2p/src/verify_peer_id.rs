use crate::multiaddress_ext::MultiaddrExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::FutureExt;
use futures::StreamExt;
use futures::TryFutureExt;
use futures::TryStreamExt;
use libp2p_core::transport::ListenerEvent;
use libp2p_core::transport::TransportError;
use libp2p_core::Multiaddr;
use libp2p_core::PeerId;
use libp2p_core::Transport;
use std::fmt;
use std::fmt::Debug;

/// A [`Transport`] wrapper that ensures we only connect to the expected peer.
///
/// Multi-addresses can contain a `/p2p` segment that specifies, which peer we want to connect to,
/// for example: `/memory/10000/p2p/12D3KooWSLdEVWR1rjrnimdX3KwTvRT8uxNs8q7keREr6MsuizJ7`
/// However, the `/p2p` part is not actually necessary to establish the physical connection. As
/// such, it can happen that a different peer sits at the end of an address like `/memory/10000`.
/// `libp2p-core` does not prevent this situation from happening. It is the responsibility of the
/// caller of [`Transport::dial`] to verify that we connected to the expected peer.
///
/// [`VerifyPeerId`] is a wrapper around [`Transport`] that performs this check for you. As a
/// trade-off, it does not allow "blind" connections to peers, i.e.: connecting to a multi-address
/// without a `/p2p` segment.
#[derive(Clone)]
pub struct VerifyPeerId<TInner> {
    inner: TInner,
}

impl<TInner> VerifyPeerId<TInner> {
    pub fn new(inner: TInner) -> Self {
        Self { inner }
    }
}

impl<TInner, C> Transport for VerifyPeerId<TInner>
where
    TInner: Transport<Output = (PeerId, C)> + 'static,
    TInner::Dial: Send + 'static,
    TInner::Listener: Send + 'static,
    TInner::ListenerUpgrade: Send + 'static,
    TInner::Error: 'static,
    C: 'static,
{
    type Output = TInner::Output;
    type Error = Error<TInner::Error>;
    #[allow(clippy::type_complexity)]
    type Listener =
        BoxStream<'static, Result<ListenerEvent<Self::ListenerUpgrade, Self::Error>, Self::Error>>;
    type ListenerUpgrade = BoxFuture<'static, Result<Self::Output, Self::Error>>;
    type Dial = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn listen_on(&mut self, addr: Multiaddr) -> Result<Self::Listener, TransportError<Self::Error>>
    where
        Self: Sized,
    {
        let listener = self
            .inner
            .listen_on(addr)
            .map_err(|e| e.map(Error::Inner))?
            .map_err(Error::Inner)
            .map_ok(|e| {
                e.map(|u| u.map_err(Error::Inner).boxed())
                    .map_err(Error::Inner)
            })
            .boxed();

        Ok(listener)
    }

    fn dial(&mut self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>>
    where
        Self: Sized,
    {
        let expected_peer_id = addr
            .clone()
            .extract_peer_id()
            .ok_or(TransportError::Other(Error::NoPeerId))?;

        let dial = self.inner.dial(addr).map_err(|e| e.map(Error::Inner))?;

        Ok(dial_and_verify_peer_id::<TInner, C>(dial, expected_peer_id).boxed())
    }

    fn dial_as_listener(
        &mut self,
        addr: Multiaddr,
    ) -> Result<Self::Dial, TransportError<Self::Error>>
    where
        Self: Sized,
    {
        let expected_peer_id = addr
            .clone()
            .extract_peer_id()
            .ok_or(TransportError::Other(Error::NoPeerId))?;
        let dial = self
            .inner
            .dial_as_listener(addr)
            .map_err(|e| e.map(Error::Inner))?;

        Ok(dial_and_verify_peer_id::<TInner, C>(dial, expected_peer_id).boxed())
    }

    fn address_translation(&self, listen: &Multiaddr, observed: &Multiaddr) -> Option<Multiaddr> {
        self.inner.address_translation(listen, observed)
    }
}

async fn dial_and_verify_peer_id<T, C>(
    dial: T::Dial,
    expected_peer_id: PeerId,
) -> Result<(PeerId, C), Error<T::Error>>
where
    T: Transport<Output = (PeerId, C)>,
{
    let (actual_peer_id, conn) = dial.await.map_err(Error::Inner)?;

    if expected_peer_id != actual_peer_id {
        return Err(Error::PeerIdMismatch {
            actual: actual_peer_id,
            expected: expected_peer_id,
        });
    }

    Ok((actual_peer_id, conn))
}

#[derive(Debug)]
pub enum Error<T> {
    PeerIdMismatch { expected: PeerId, actual: PeerId },
    NoPeerId,
    Inner(T),
}

impl<T: fmt::Display> fmt::Display for Error<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::PeerIdMismatch { actual, expected } => {
                write!(f, "Peer ID mismatch, expected {expected} but got {actual}")
            }
            Error::Inner(_) => Ok(()),
            Error::NoPeerId => write!(f, "The given address does not contain a peer ID"),
        }
    }
}

impl<T> std::error::Error for Error<T>
where
    T: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::PeerIdMismatch { .. } => None,
            Error::NoPeerId => None,
            Error::Inner(inner) => Some(inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p_core::transport::memory::Channel;
    use libp2p_core::transport::MemoryTransport;
    use libp2p_core::ConnectedPoint;

    #[test]
    fn rejects_address_without_peer_id() {
        let mut transport =
            VerifyPeerId::new(MemoryTransport::default().map(simulate_auth_upgrade));

        let result = transport.dial("/memory/10000".parse().unwrap());

        assert!(matches!(
            result,
            Err(TransportError::Other(Error::NoPeerId))
        ))
    }

    #[tokio::test]
    async fn fails_dial_with_unexpected_peer_id() {
        let _alice = MemoryTransport::default()
            .map(simulate_auth_upgrade)
            .listen_on("/memory/10000".parse().unwrap())
            .unwrap();
        let mut bob = VerifyPeerId::new(MemoryTransport::default().map(simulate_auth_upgrade));

        let result = bob
            .dial(
                // Hardcoded PeerId will unlikely be equal to random one that is simulated in auth
                // upgrade.
                "/memory/10000/p2p/12D3KooWSLdEVWR1rjrnimdX3KwTvRT8uxNs8q7keREr6MsuizJ7"
                    .parse()
                    .unwrap(),
            )
            .unwrap()
            .await;

        assert!(matches!(result, Err(Error::PeerIdMismatch { .. })))
    }

    // Mapping function for simulating an authentication upgrade in a transport.
    fn simulate_auth_upgrade(
        conn: Channel<Vec<u8>>,
        _: ConnectedPoint,
    ) -> (PeerId, Channel<Vec<u8>>) {
        (PeerId::random(), conn)
    }
}
