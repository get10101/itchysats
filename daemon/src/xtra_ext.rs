use async_trait::async_trait;
use std::fmt;
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
