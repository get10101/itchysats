use crate::protocol;
use async_trait::async_trait;
use tokio_extras::Tasks;
use xtra_libp2p::NewInboundSubstream;
use xtra_productivity::xtra_productivity;

#[derive(Default)]
pub struct Actor {
    tasks: Tasks,
}

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle(&mut self, message: NewInboundSubstream) {
        let NewInboundSubstream { stream, peer } = message;

        let future = protocol::recv(stream);

        self.tasks.add_fallible(future, move |e| async move {
            tracing::debug!(%peer, "Inbound ping protocol failed: {e}");
        });
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
