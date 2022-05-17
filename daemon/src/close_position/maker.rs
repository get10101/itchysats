use anyhow::anyhow;
use anyhow::Context;
use anyhow::Error;
use anyhow::Result;
use async_trait::async_trait;
use asynchronous_codec::Framed;
use asynchronous_codec::JsonCodec;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::PeerId;
use model::ClosePositionTransaction;
use model::CollaborativeSettlement;
use model::OrderId;
use model::SettlementProposal;
use std::collections::HashMap;
use tokio_tasks::Tasks;
use xtra_libp2p::NewInboundSubstream;
use xtra_libp2p::Substream;
use xtra_productivity::xtra_productivity;

use crate::close_position::protocol::*;
use crate::command;

/// Permanent actor to handle incoming substreams for the `/itchysats/close-position/1.0.0`
/// protocol.
///
/// There is only one instance of this actor for all connections, meaning we must always spawn a
/// task whenever we interact with a substream to not block the execution of other connections.
pub struct Actor {
    tasks: Tasks,
    pending_protocols: HashMap<
        OrderId,
        (
            Framed<Substream, JsonCodec<ListenerMessage, DialerMessage>>,
            ClosePositionTransaction,
            SettlementProposal,
            PeerId,
        ),
    >,
    executor: command::Executor,
    n_payouts: usize,
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

// 10 rollovers
// - NewInboundStream
// - Msg0Received
// - NewInboundStream
// - Msg0Received
// - NewInboundStream
// - Msg0Received
// - NewInboundStream
// - Msg0Received
// - NewInboundStream
// - Msg0Received
// - NewInboundStream
// - Msg0Received
// - NewInboundStream
// - Msg0Received
// - NewInboundStream
// - Msg0Received
// - NewInboundStream

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle(&mut self, msg: NewInboundSubstream, ctx: &mut xtra::Context<Self>) {
        let NewInboundSubstream { peer, stream } = msg; // TODO: Use `PeerId` for something!
        let address = ctx.address().expect("we are alive");

        self.tasks.add_fallible(
            async move {
                let mut framed =
                    Framed::new(stream, JsonCodec::<ListenerMessage, DialerMessage>::new());

                let msg0 = framed
                    .next()
                    .await
                    .context("End of stream while receiving Msg0")?
                    .context("Failed to decode Msg0")?
                    .into_msg0()?;

                address.send(Msg0Received { msg0, framed, peer }).await?;

                anyhow::Ok(())
            },
            |e| async move { tracing::warn!("Failed to handle incoming close position: {e:#}") },
        );
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: Msg0Received) {
        let Msg0Received { msg0, framed, peer } = msg;
        let order_id = msg0.id;

        let result = self
            .executor
            .execute(order_id, |cfd| {
                // TODO: Validate that the correct peer is invoking this?
                cfd.start_close_position_maker(msg0.price, self.n_payouts, msg0.unsigned_tx)
            })
            .await
            .context("Failed to start close position protocol");

        let (transaction, proposal) = match result {
            Ok((transaction, proposal)) => (transaction, proposal),
            Err(e) => {
                tracing::debug!(%order_id, %peer, "Failed to start close position protocol: {e:#}");
                return;
            }
        };

        self.pending_protocols
            .insert(order_id, (framed, transaction, proposal, peer));
    }

    async fn handle(&mut self, msg: Accept, ctx: &mut xtra::Context<Self>) -> Result<()> {
        let Accept { order_id } = msg;
        let address = ctx.address().expect("we are alive");

        let (mut framed, transaction, proposal, peer) = self
            .pending_protocols
            .remove(&order_id)
            .with_context(|| format!("No active protocol for order {order_id}"))?;

        self.executor
            .execute(order_id, |cfd| {
                cfd.accept_collaborative_settlement_proposal(&proposal)
            })
            .await?;

        self.tasks.add_fallible(
            async move {
                framed
                    .send(ListenerMessage::Msg1(Msg1::Accept))
                    .await
                    .context("Failed to send Msg1::Accept")?;

                // TODO: We will need to apply a timeout to these. Perhaps we can put a timeout
                // generally into "reading from the substream"?
                let Msg2 { dialer_signature } = framed
                    .next()
                    .await
                    .context("End of stream while receiving Msg2")?
                    .context("Failed to decode Msg2")?
                    .into_msg2()?;

                let listener_signature = transaction.own_signature();

                let settlement = transaction
                    .recv_counterparty_signature(dialer_signature)
                    .context("Failed to receive counterparty signature")?
                    .finalize()
                    .context("Failed to finalize transaction")?;

                    framed.send(ListenerMessage::Msg3(Msg3 {
                        listener_signature,
                    }))
                    .await
                    .map_err(|source| Failed::AfterReceiving {
                        source: anyhow!(source),
                        settlement: settlement.clone(),
                    })?;

                address
                    .send(Complete {
                        order_id,
                        settlement: settlement.clone(),
                    })
                    .await
                    .map_err(|source| Failed::AfterReceiving {
                        source: anyhow!(source),
                        settlement,
                    })?;

                Ok(())
            },
{
    let executor = self.executor.clone();
    move |failed|                 async move {
        match failed {
            Failed::BeforeReceiving { source } => {
                executor
                    .execute(order_id, |cfd| {
                        Ok(cfd.fail_collaborative_settlement(source))
                    })
                    .await
                    .expect("TODO: How to handle failure in executing command?");
            }
            Failed::AfterReceiving {
                source,
                settlement,
            } => {
                tracing::debug!("Proceeding with transaction despite failure to complete settlement after receiving signature from taker: {source:#}");
                if let Err(e) =
                executor
                    .execute(order_id, |cfd| {
                        Ok(cfd.complete_collaborative_settlement(settlement))
                    })
                    .await {
                        tracing::error!("Failed to complete settlement: {e:#}");
                    };
                }
            }
        }
    }
        );

        // 1. Spawn async fn into tasks to perform further protocol steps like sending and
        // receiving.
        //
        // 2. Notify actor at end of protocol to save state

        Ok(())
    }

    async fn handle(
        &mut self,
        Complete {
            order_id,
            settlement,
        }: Complete,
    ) -> Result<()> {
        self.executor
            .execute(order_id, |cfd| {
                Ok(cfd.complete_collaborative_settlement(settlement))
            })
            .await
    }

    async fn handle(&mut self, msg: Reject) -> Result<()> {
        let Reject { order_id } = msg;

        let (mut framed, ..) = self
            .pending_protocols
            .remove(&order_id)
            .with_context(|| format!("No active protocol for order {order_id}"))?;

        self.executor
            .execute(order_id, |cfd| {
                Ok(cfd.reject_collaborative_settlement(anyhow!("maker decision")))
            })
            .await?;

        self.tasks.add_fallible(
            async move { framed.send(ListenerMessage::Msg1(Msg1::Reject)).await },
            move |e| async move {
                tracing::debug!(%order_id, "Failed to reject collaborative settlement")
            },
        );

        Ok(())
    }
}

struct Msg0Received {
    msg0: Msg0,
    framed: Framed<Substream, JsonCodec<ListenerMessage, DialerMessage>>,
    peer: PeerId,
}

pub struct Accept {
    order_id: OrderId,
}

pub struct Reject {
    order_id: OrderId,
}

pub struct Complete {
    order_id: OrderId,
    settlement: CollaborativeSettlement,
}

#[derive(Debug)]
enum Failed {
    BeforeReceiving {
        source: Error,
    },
    AfterReceiving {
        settlement: CollaborativeSettlement,
        source: Error,
    },
}

// Allows for easy use of `?`.
impl From<Error> for Failed {
    fn from(source: Error) -> Self {
        Failed::BeforeReceiving { source }
    }
}
