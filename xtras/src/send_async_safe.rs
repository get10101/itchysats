use async_trait::async_trait;
use std::fmt;

#[async_trait]
pub trait SendAsyncSafe<M: Send + 'static, R> {
    /// Send a message to an actor without waiting for them to handle
    /// it.
    ///
    /// As soon as this method returns, we know if the receiving actor
    /// is alive. If they are, the message we are sending will
    /// eventually be handled by them, but we don't wait for them to
    /// do so.
    async fn send_async_safe(&self, msg: M) -> Result<(), xtra::Error>;
}

#[async_trait]
impl<A, M> SendAsyncSafe<M, ()> for xtra::Address<A>
where
    A: xtra::Handler<M, Return = ()>,
    M: Send + 'static,
{
    async fn send_async_safe(&self, msg: M) -> Result<(), xtra::Error> {
        todo!("split_receiver() should resolve to Result<Receiver<..>, Error>") // TODO(restioson)
        // #[allow(clippy::disallowed_methods)]
        // self.send(msg).split_receiver().await
    }
}

#[async_trait]
impl<A, M, E> SendAsyncSafe<M, Result<(), E>> for xtra::Address<A>
where
    A: xtra::Handler<M, Return = Result<(), E>>,
    E: fmt::Display + Send + 'static,
    M: Send + 'static,
{
    // TODO what is this doing?
    async fn send_async_safe(&self, msg: M) -> Result<(), xtra::Error> {
        if !self.is_connected() {
            return Err(xtra::Error::Disconnected);
        }

        let send_fut = self.send(msg);

        #[allow(clippy::disallowed_methods)]
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
impl<M> SendAsyncSafe<M, ()> for Box<dyn xtra::prelude::MessageChannel<M, Return = ()>>
    where M: Send + 'static
{
    async fn send_async_safe(&self, msg: M) -> Result<(), xtra::Error> {
        todo!("split_receiver() should resolve to Result<Receiver<..>, Error>")
        // #[allow(clippy::disallowed_methods)]
        // self.send(msg).split_receiver().await
    }
}

#[async_trait]
impl<M> SendAsyncSafe<M, ()> for Box<dyn xtra::prelude::StrongMessageChannel<M, Return = ()>>
    where M: Send + 'static
{
    async fn send_async_safe(&self, msg: M) -> Result<(), xtra::Error> {
        todo!("split_receiver() should resolve to Result<Receiver<..>, Error>")
        // #[allow(clippy::disallowed_methods)]
        // self.send(msg).split_receiver().await
    }
}

#[async_trait]
impl<M, E> SendAsyncSafe<M, Result<(), E>> for Box<dyn xtra::prelude::MessageChannel<M, Return = Result<(), E>>>
where
    E: fmt::Display + Send + 'static,
    M: Send + 'static
{
    async fn send_async_safe(&self, msg: M) -> Result<(), xtra::Error> {
        if !self.is_connected() {
            return Err(xtra::Error::Disconnected);
        }

        let send_fut = self.send(msg);

        #[allow(clippy::disallowed_methods)]
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
