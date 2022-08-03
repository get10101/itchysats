use crate::protocol;
use async_trait::async_trait;
use xtra::Context;
use xtra_libp2p::NewInboundSubstream;
use xtra_productivity::xtra_productivity;

#[derive(Copy, Clone)]
pub struct Actor;

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, message: NewInboundSubstream, ctx: &mut Context<Self>) {
        let NewInboundSubstream { stream, peer } = message;

        let future = protocol::recv(stream);

        tokio_extras::spawn_fallible(
            &ctx.address().expect("self to be alive"),
            future,
            move |e| async move {
                tracing::warn!(%peer, "Inbound ping protocol failed: {e}");
            },
        );
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
