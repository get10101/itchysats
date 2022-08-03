use crate::collab_settlement::protocol::*;
use crate::command;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use model::libp2p::PeerId;
use model::OrderId;
use model::Price;
use xtra::Address;
use xtra_libp2p::Endpoint;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    endpoint: Address<Endpoint>,
    executor: command::Executor,
    n_payouts: usize,
}

impl Actor {
    pub fn new(endpoint: Address<Endpoint>, executor: command::Executor, n_payouts: usize) -> Self {
        Self {
            endpoint,
            executor,
            n_payouts,
        }
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

#[derive(Clone, Copy)]
pub struct Settle {
    pub order_id: OrderId,
    pub price: Price,
    pub maker_peer_id: PeerId,
}

#[xtra_productivity]
impl Actor {
    pub async fn handle(&mut self, msg: Settle, ctx: &mut xtra::Context<Self>) -> Result<()> {
        let Settle {
            order_id,
            price,
            maker_peer_id,
        } = msg;

        let (collab_settlement_tx, _) = self
            .executor
            .execute(order_id, |cfd| {
                cfd.start_collab_settlement_taker(price, self.n_payouts)
            })
            .await
            .context("could not start closing position")?;

        tokio_extras::spawn_fallible(
            &ctx.address().expect("self to be alive"),
            {
                let endpoint = self.endpoint.clone();
                let executor = self.executor.clone();
                async move {
                    let settlement = dialer(
                        endpoint,
                        order_id,
                        maker_peer_id.inner(),
                        collab_settlement_tx.clone(),
                    )
                    .await?;

                    emit_completed(order_id, settlement, &executor).await;
                    Ok(())
                }
            },
            {
                let executor = self.executor.clone();
                move |e| async move {
                    match e {
                        e @ DialerFailed::AfterSendingSignature { .. } => {
                            // TODO: We should start monitoring whether other party published the
                            // transaction
                            emit_failed(order_id, anyhow!(e), &executor).await;
                        }
                        e @ DialerFailed::BeforeSendingSignature { .. } => {
                            emit_failed(order_id, anyhow!(e), &executor).await;
                        }
                        DialerFailed::Rejected => {
                            emit_rejected(order_id, &executor).await;
                        }
                    }
                }
            },
        );

        Ok(())
    }
}
