use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use tokio::time::error::Elapsed;
use tokio::time::Timeout as TokioTimeout;
use tracing::field;
use tracing::instrument::Instrumented;
use tracing::Instrument;
use tracing::Span;

#[tracing::instrument(name = "Sleep")]
pub async fn sleep(duration: Duration) {
    #[allow(clippy::disallowed_methods)]
    tokio::time::sleep(duration).await
}

/// Limit the future's time of execution to a certain duration, cancelling it and returning
/// an error if time runs out. This is instrumented, unlike `tokio::time::timeout`. The
/// `child_span` function constructs the span for the child future from the span of the parent
/// (timeout) future.
pub fn timeout<F>(duration: Duration, fut: F, child_span: impl FnOnce(&Span) -> Span) -> Timeout<F>
where
    F: Future,
{
    let parent = tracing::debug_span!(
        "Future with timeout",
        timeout_secs = duration.as_secs(),
        timed_out = field::Empty
    );

    #[allow(clippy::disallowed_methods)]
    Timeout {
        fut: tokio::time::timeout(duration, fut).instrument(child_span(&parent)),
        span: parent,
    }
}

/// Child-span constructor to pass to [`timeout`] or [`crate::future_ext::FutureExt::timeout`]
/// if the future being timed out is already instrumented.
pub fn already_instrumented(parent: &Span) -> Span {
    parent.clone()
}

pin_project_lite::pin_project! {
    pub struct Timeout<F> {
        #[pin]
        fut: Instrumented<TokioTimeout<F>>,
        span: Span,
    }
}

impl<F> Future for Timeout<F>
where
    F: Future,
{
    type Output = Result<F::Output, Elapsed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _enter = this.span.enter();
        let poll = this.fut.poll(cx);

        match poll {
            Poll::Ready(Ok(_)) => {
                this.span.record("timed_out", &false);
                poll
            }
            Poll::Ready(Err(ref e)) => {
                tracing::error!(err = %e, "Future timed out");
                this.span.record("timed_out", &true);
                poll
            }
            _ => poll,
        }
    }
}
