use std::fmt;

use xtra::prelude::MessageChannel;
use xtra::{Handler, KeepRunning};

/// A forwarding actor that only forwards [`Result::Ok`] values and shuts itself down upon the first
/// error.
pub struct Actor<M> {
    forward: Box<dyn MessageChannel<M>>,
}

impl<M> Actor<M> {
    pub fn new(forward: Box<dyn MessageChannel<M>>) -> Self {
        Self { forward }
    }
}

pub struct Message<TOk, TErr>(pub Result<TOk, TErr>);

impl<TOk, TErr> xtra::Message for Message<TOk, TErr>
where
    TOk: Send + 'static,
    TErr: Send + 'static,
{
    type Result = KeepRunning;
}

#[async_trait::async_trait]
impl<TOk, TErr> Handler<Message<TOk, TErr>> for Actor<TOk>
where
    TOk: xtra::Message<Result = ()> + Send + 'static,
    TErr: fmt::Display + Send + 'static,
{
    async fn handle(
        &mut self,
        Message(result): Message<TOk, TErr>,
        _: &mut xtra::Context<Self>,
    ) -> KeepRunning {
        let ok = match result {
            Ok(ok) => ok,
            Err(e) => {
                tracing::error!("Stopping forwarding due to error: {}", e);

                return KeepRunning::StopSelf;
            }
        };

        if let Err(xtra::Disconnected) = self.forward.send(ok).await {
            tracing::info!("Target actor disappeared, stopping");

            return KeepRunning::StopSelf;
        }

        KeepRunning::Yes
    }
}

impl<T: 'static + Send> xtra::Actor for Actor<T> {}
