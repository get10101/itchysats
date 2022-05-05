use async_trait::async_trait;
use std::fmt;
use xtra::address;
use xtra::message_channel;
use xtra::Actor;
use xtra::Error;
use xtra::Message;

#[async_trait]
pub trait LogFailure {
    async fn log_failure(self, context: &str) -> Result<(), Error>;
}

#[async_trait]
impl<A, M> LogFailure for address::SendFuture<A, M>
where
    A: Actor,
    M: Message<Result = anyhow::Result<()>>,
{
    async fn log_failure(self, context: &str) -> Result<(), Error> {
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
    M: Message<Result = anyhow::Result<(), E>>,
    E: fmt::Display + Send,
{
    async fn log_failure(self, context: &str) -> Result<(), Error> {
        if let Err(e) = self.await? {
            let type_name = std::any::type_name::<M>();

            tracing::warn!("{context}: Message handler for message {type_name} failed: {e:#}");
        }

        Ok(())
    }
}
