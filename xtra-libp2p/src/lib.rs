pub use crate::endpoint::Connect;
pub use crate::endpoint::ConnectionStats;
pub use crate::endpoint::Disconnect;
pub use crate::endpoint::Endpoint;
pub use crate::endpoint::Error;
pub use crate::endpoint::GetConnectionStats;
pub use crate::endpoint::ListenOn;
pub use crate::endpoint::Multiple;
pub use crate::endpoint::NewInboundSubstream;
pub use crate::endpoint::OpenSubstream;
pub use crate::endpoint::Single;
pub use crate::substream::Substream;
pub use libp2p_core as libp2p;
pub use multistream_select::NegotiationError;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use libp2p_core::Negotiated;
use libp2p_core::PeerId;

pub mod dialer;
pub mod endpoint;
pub mod listener;
pub mod multiaddress_ext;
mod substream;
mod upgrade;
mod verify_peer_id;

type Connection = (
    PeerId,
    yamux::Control,
    BoxStream<
        'static,
        Result<
            Result<(Negotiated<yamux::Stream>, &'static str), upgrade::Error>,
            yamux::ConnectionError,
        >,
    >,
    BoxFuture<'static, ()>,
);
