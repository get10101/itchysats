use xtra::prelude::MessageChannel;

/// A fan-out actor takes every incoming message and forwards it to a set of other actors.
#[derive(xtra_productivity::Actor)]
pub struct Actor<M>
where
    M: xtra::Message<Result = ()>,
{
    receivers: Vec<Box<dyn MessageChannel<M>>>,
}

impl<M> Actor<M>
where
    M: xtra::Message<Result = ()>,
{
    pub fn new(receivers: &[&dyn MessageChannel<M>]) -> Self {
        Self {
            receivers: receivers.iter().map(|c| c.clone_channel()).collect(),
        }
    }
}

#[async_trait::async_trait]
impl<M> xtra::Handler<M> for Actor<M>
where
    M: xtra::Message<Result = ()> + Clone + Sync + 'static,
{
    async fn handle(&mut self, message: M, _: &mut xtra::Context<Self>) {
        for receiver in &self.receivers {
            // Not sure why here is no `do_send_async` ...
            let _ = receiver.do_send(message.clone());
        }
    }
}
