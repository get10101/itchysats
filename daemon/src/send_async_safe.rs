use async_trait::async_trait;
use std::fmt;

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
