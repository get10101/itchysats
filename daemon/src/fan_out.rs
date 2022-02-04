use async_trait::async_trait;
use xtra::prelude::MessageChannel;

/// A fan-out actor takes every incoming message and forwards it to a set of other actors.
pub struct Actor<M: xtra::Message<Result = ()>> {
    receivers: Vec<Box<dyn MessageChannel<M>>>,
}

impl<M: xtra::Message<Result = ()>> Actor<M> {
    pub fn new(receivers: &[&dyn MessageChannel<M>]) -> Self {
        Self {
            receivers: receivers.iter().map(|c| c.clone_channel()).collect(),
        }
    }
}

#[async_trait]
impl<M> xtra::Actor for Actor<M>
where
    M: xtra::Message<Result = ()>,
{
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

#[async_trait::async_trait]
impl<M> xtra::Handler<M> for Actor<M>
where
    M: xtra::Message<Result = ()> + Clone + Sync + 'static,
{
    async fn handle(&mut self, message: M, _: &mut xtra::Context<Self>) {
        for receiver in &self.receivers {
            if receiver.send(message.clone()).await.is_err() {
                // Should ideally remove from list but that is unnecessarily hard with Rust
                // iterators
                tracing::error!("Actor disconnected, cannot send message");
            }
        }
    }
}
