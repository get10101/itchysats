use async_trait::async_trait;
use std::time::Duration;
use tracing::Instrument;
use xtra::address;

#[async_trait]
pub trait SendInterval<A, M>
where
    A: xtra::Handler<M>,
{
    /// Similar to xtra::Context::notify_interval, however it uses `send`
    /// instead of `do_send` under the hood.
    /// The crucial difference is that this function waits until previous
    /// handler returns before scheduling a new one, thus preventing them from
    /// piling up.
    /// As a bonus, this function is non-fallible.
    async fn send_interval<F>(self, duration: Duration, constructor: F)
    where
        F: Send + Sync + Fn() -> M,
        A: xtra::Handler<M, Return = ()>;
}

#[async_trait]
impl<A, M> SendInterval<A, M> for address::Address<A>
where
    A: xtra::Handler<M>,
    M: Send + 'static,
{
    async fn send_interval<F>(self, duration: Duration, constructor: F)
    where
        F: Send + Sync + Fn() -> M,
    {
        let span = || {
            tracing::debug_span!(
                "Send message every interval",
                interval_secs = %duration.as_secs()
            )
        };

        while self.send(constructor()).instrument(span()).await.is_ok() {
            tokio_extras::time::sleep_silent(duration).await
        }
        let type_name = std::any::type_name::<M>();

        tracing::warn!(
            "Task for periodically sending message {type_name} stopped because actor shut down"
        );
    }
}
