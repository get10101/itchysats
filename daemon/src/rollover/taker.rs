use crate::command;
use crate::rollover::protocol::dialer;
use anyhow::anyhow;
use anyhow::Result;
use async_trait::async_trait;
use libp2p_core::PeerId;
use model::OrderId;
use tokio_tasks::Tasks;
use xtra::Address;
use xtra_libp2p::Endpoint;
use xtra_productivity::xtra_productivity;

/// One actor to rule all the rollovers
pub struct Actor {
    endpoint: Address<Endpoint>,
    tasks: Tasks,
    executor: command::Executor,
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

pub struct ProposeRollover {
    pub id: OrderId,
}

#[xtra_productivity]
impl Actor {
    pub async fn handle(
        &mut self,
        msg: ProposeRollover,
        ctx: &mut xtra::Context<Self>,
    ) -> Result<()> {
        let this = ctx.address().expect("we are alive");

        let ProposeRollover { id } = msg;

        self.executor
            .execute(id, |cfd| cfd.start_rollover())
            .await?;

        let maker = PeerId::random(); // TODO: This needs to be returned from the above `execute` call as well.

        dialer(self.endpoint.clone(), id, maker).await?;

        Ok(())
    }
}