use conquer_once::Lazy;
use futures::ready;
use futures::AsyncRead;
use futures::AsyncWrite;
use libp2p_core::Endpoint;
use libp2p_core::Negotiated;
use pin_project::pin_project;
use prometheus::HistogramTimer;
use prometheus::IntCounter;
use std::collections::HashMap;
use std::fmt;
use std::fmt::Debug;
use std::io::IoSlice;
use std::io::IoSliceMut;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

/// A substream is an isolated channel within another connection.
///
/// The isolation is achieved through multiplexing by means of yamux.
///
/// Opening a new substream is cheap compared to say a TCP connection. Additionally, because
/// substreams are opened on top of an existing connection, they are not affected by NAT or
/// firewalls. Once a connection is established, either end of the connection (the original dialer
/// and listener) can open substreams.
///
/// Each substream is dedicated to a specific protocol which must be specified upon construction.
///
/// Substreams are instrumented with prometheus metrics that track the duration they are alive for
/// and how many bytes are read from and written to the stream.
#[pin_project]
pub struct Substream {
    #[pin]
    inner: Negotiated<yamux::Stream>,

    /// The prometheus timer tracking the duration of the substream.
    ///
    /// This timer is started upon construction and automatically stops once it is dropped. Thus,
    /// we only need to keep it around but not interact with it after construction.
    _timer: HistogramTimer,

    /// The prometheus counter for the number of bytes read.
    read_counter: IntCounter,

    /// The prometheus counter for the number of bytes written.
    written_counter: IntCounter,
}

impl Debug for Substream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Substream")
            .field("inner", &self.inner)
            .finish()
    }
}

impl Substream {
    pub(crate) fn new(
        inner: Negotiated<yamux::Stream>,
        protocol: &'static str,
        role: Endpoint,
    ) -> Self {
        let role = match role {
            Endpoint::Dialer => "dialer",
            Endpoint::Listener => "listener",
        };
        let labels = HashMap::from([(PROTOCOL_LABEL, protocol), (ROLE_LABEL, role)]);

        Self {
            inner,
            _timer: SUBSTREAM_DURATION_HISTOGRAM.with(&labels).start_timer(),
            read_counter: SUBSTREAM_BYTES_READ_COUNTER.with(&labels),
            written_counter: SUBSTREAM_BYTES_WRITTEN_COUNTER.with(&labels),
        }
    }
}

impl AsyncRead for Substream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.project();

        let bytes_read = ready!(this.inner.poll_read(cx, buf)?);
        this.read_counter.inc_by(bytes_read as u64);

        Poll::Ready(Ok(bytes_read))
    }

    fn poll_read_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.project();

        let bytes_read = ready!(this.inner.poll_read_vectored(cx, bufs)?);
        this.read_counter.inc_by(bytes_read as u64);

        Poll::Ready(Ok(bytes_read))
    }
}

impl AsyncWrite for Substream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.project();

        let bytes_written = ready!(this.inner.poll_write(cx, buf)?);
        this.written_counter.inc_by(bytes_written as u64);

        Poll::Ready(Ok(bytes_written))
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.project();

        let bytes_written = ready!(this.inner.poll_write_vectored(cx, bufs)?);
        this.written_counter.inc_by(bytes_written as u64);

        Poll::Ready(Ok(bytes_written))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.project().inner.poll_close(cx)
    }
}

const PROTOCOL_LABEL: &str = "protocol";

/// The role of substream in the protocol: dialer or listener.
const ROLE_LABEL: &str = "role";

static SUBSTREAM_DURATION_HISTOGRAM: Lazy<prometheus::HistogramVec> = Lazy::new(|| {
    prometheus::register_histogram_vec!(
        "substream_duration_seconds",
        "The duration of a protocol's substream in seconds.",
        &[PROTOCOL_LABEL, ROLE_LABEL],
        vec![0.5, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 15.0, 20.0, 30.0, 50.0, 100.0]
    )
    .unwrap()
});

static SUBSTREAM_BYTES_READ_COUNTER: Lazy<prometheus::IntCounterVec> = Lazy::new(|| {
    prometheus::register_int_counter_vec!(
        "substream_read_bytes",
        "The total number of bytes read from substreams, segregated by protocol.",
        &[PROTOCOL_LABEL, ROLE_LABEL]
    )
    .unwrap()
});

static SUBSTREAM_BYTES_WRITTEN_COUNTER: Lazy<prometheus::IntCounterVec> = Lazy::new(|| {
    prometheus::register_int_counter_vec!(
        "substream_written_bytes",
        "The total number of bytes written to substreams, segregated by protocol.",
        &[PROTOCOL_LABEL, ROLE_LABEL]
    )
    .unwrap()
});
