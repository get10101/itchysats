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
            let type_name = std::any::type_name::<M>();

            tracing::warn!("{context}: Message handler for message {type_name} failed: {e:#}");
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
            let type_name = std::any::type_name::<M>();

            tracing::warn!("{context}: Message handler for message {type_name} failed: {e:#}");
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
        let type_name = std::any::type_name::<M>();

        tracing::warn!(
            "Task for periodically sending message {type_name} stopped because actor shut down"
        );
    }
}

pub trait ActorName {
    fn name() -> String;
}

impl<T> ActorName for T
where
    T: xtra::Actor,
{
    /// Devise the name of an actor from its type on a best-effort basis.
    ///
    /// To reduce some noise, we strip `daemon::` out of all module names contained in the type.
    fn name() -> String {
        std::any::type_name::<T>().replace("daemon::", "")
    }
}

#[async_trait]
pub trait SendAsyncSafe<M, R>
where
    M: xtra::Message<Result = R>,
{
    /// Send a message to an actor without waiting for them to handle
    /// it.
    ///
    /// As soon as this method returns, we know if the receiving actor
    /// is alive. If they are, the message we are sending will
    /// eventually be handled by them, but we don't wait for them to
    /// do so.
    async fn send_async_safe(&self, msg: M) -> Result<(), xtra::Disconnected>;
}

#[async_trait]
impl<A, M> SendAsyncSafe<M, ()> for xtra::Address<A>
where
    A: xtra::Handler<M>,
    M: xtra::Message<Result = ()>,
{
    async fn send_async_safe(&self, msg: M) -> Result<(), xtra::Disconnected> {
        #[allow(clippy::disallowed_method)]
        self.do_send_async(msg).await
    }
}

#[async_trait]
impl<A, M, E> SendAsyncSafe<M, Result<(), E>> for xtra::Address<A>
where
    A: xtra::Handler<M>,
    M: xtra::Message<Result = Result<(), E>>,
    E: fmt::Display + Send,
{
    async fn send_async_safe(&self, msg: M) -> Result<(), xtra::Disconnected> {
        if !self.is_connected() {
            return Err(xtra::Disconnected);
        }

        let send_fut = self.send(msg);

        #[allow(clippy::disallowed_method)]
        tokio::spawn(async {
            let e = match send_fut.await {
                Ok(Err(e)) => format!("{e:#}"),
                Err(e) => format!("{e:#}"),
                Ok(Ok(())) => return,
            };

            tracing::warn!("Async message invocation failed: {:#}", e)
        });

        Ok(())
    }
}

#[async_trait]
impl<M, E> SendAsyncSafe<M, Result<(), E>> for Box<dyn xtra::prelude::MessageChannel<M>>
where
    M: xtra::Message<Result = Result<(), E>>,
    E: fmt::Display + Send,
{
    async fn send_async_safe(&self, msg: M) -> Result<(), xtra::Disconnected> {
        if !self.is_connected() {
            return Err(xtra::Disconnected);
        }

        let send_fut = self.send(msg);

        #[allow(clippy::disallowed_method)]
        tokio::spawn(async {
            let e = match send_fut.await {
                Ok(Err(e)) => format!("{e:#}"),
                Err(e) => format!("{e:#}"),
                Ok(Ok(())) => return,
            };

            tracing::warn!("Async message invocation failed: {e:#}")
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_name_from_type() {
        let name = Dummy::name();

        assert_eq!(name, "xtra_ext::tests::Dummy")
    }

    struct Dummy;

    impl xtra::Actor for Dummy {}
}
