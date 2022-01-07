use async_trait::async_trait;
use std::fmt;
use std::time::Duration;
use xtra::address;
use xtra::message_channel;
use xtra::Actor;
use xtra::Disconnected;
use xtra::Message;

#[async_trait]
pub trait LogFailure {
    async fn log_failure(self, context: &str) -> Result<(), Disconnected>;
}

#[async_trait]
impl<A, M> LogFailure for address::SendFuture<A, M>
where
    A: Actor,
    M: Message<Result = anyhow::Result<()>>,
{
    async fn log_failure(self, context: &str) -> Result<(), Disconnected> {
        if let Err(e) = self.await? {
            tracing::warn!(
                "{}: Message handler for message {} failed: {:#}",
                context,
                std::any::type_name::<M>(),
                e
            );
        }

        Ok(())
    }
}

#[async_trait]
impl<M, E> LogFailure for message_channel::SendFuture<M>
where
    M: xtra::Message<Result = anyhow::Result<(), E>>,
    E: fmt::Display + Send,
{
    async fn log_failure(self, context: &str) -> Result<(), Disconnected> {
        if let Err(e) = self.await? {
            tracing::warn!(
                "{}: Message handler for message {} failed: {:#}",
                context,
                std::any::type_name::<M>(),
                e
            );
        }

        Ok(())
    }
}

#[async_trait]
pub trait SendInterval<A, M>
where
    M: Message,
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
        M: Message<Result = ()>,
        A: xtra::Handler<M>;
}

#[async_trait]
impl<A, M> SendInterval<A, M> for address::Address<A>
where
    M: Message,
    A: xtra::Handler<M>,
{
    async fn send_interval<F>(self, duration: Duration, constructor: F)
    where
        F: Send + Sync + Fn() -> M,
    {
        while self.send(constructor()).await.is_ok() {
            tokio::time::sleep(duration).await
        }
        tracing::warn!(
            "Task for periodically sending message {} stopped because actor shut down",
            std::any::type_name::<M>()
        );
    }
}
