use crate::close_position::protocol::ListenerMessage::Msg3;
use crate::close_position::protocol::Msg1::Reject;
use crate::close_position::protocol::*;
use crate::command;
use anyhow::anyhow;
use anyhow::bail;
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

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle(&mut self, msg: NewInboundSubstream, ctx: &mut xtra::Context<Self>) {
        let NewInboundSubstream { peer, stream } = msg; // TODO: Use `PeerId` for something!
        let address = ctx.address().expect("we are alive");

        // 1. Read message from stream but this can be blocking so needs to be in a separate task ->
        // send message to ourselves once received

        self.tasks.add_fallible(
            async move {
                let mut framed = asynchronous_codec::Framed::new(
                    stream,
                    JsonCodec::<ListenerMessage, DialerMessage>::new(),
                );

                let msg0 = framed
                    .next()
                    .await
                    .expect("TODO")
                    .expect("TODO")
                    .into_msg0()
                    .expect("TODO");

                let _ = address.send(Msg0Received { msg0, framed, peer }).await;
            },
            |e| async move { todo!(e) },
        )
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

        let executor = self.executor.clone();

        executor
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

                let Msg2 { dialer_signature } = framed
                    .next()
                    .await
                    .context("End of stream while receiving Msg2")?
                    .context("Failed to receive Msg2")?;

                let transaction = transaction
                    .recv_counterparty_signature(dialer_signature)
                    .context("Failed to receive counterparty signature")?;

                framed
                    .send(ListenerMessage::Msg3(Msg3 {
                        listener_signature: transaction.own_signature(),
                    }))
                    .await
                    .map_err(|source| Failed::AfterReceiving {
                        source,
                        transaction: transaction.clone(),
                    })?;

                address
                    .send(Complete { settlement })
                    .await
                    .map_err(|source| Failed::AfterReceiving {
                        source: anyhow!(source),
                        transaction,
                    })?;

                Ok(())
            },
            |failed| async move {
                match failed {
                    Failed::BeforeReceiving { source } => {
                        executor
                            .execute(
                                order_id,
                                |cfd| Ok(cfd.fail_collaborative_settlement(source)),
                            )
                            .await
                            .expect("TODO: How to handle failure in executing command?");
                    }
                    Failed::AfterReceiving {
                        source,
                        transaction,
                    } => {
                        // TODO: Decide to broadcast the transaction anyway? Technically we are
                        // complete from the protocol from our perspective, we just failed in
                        // sending the last message.
                    }
                }
            },
        );

        // 1. Spawn async fn into tasks to perform further protocol steps like sending and
        // receiving.
        //
        // 2. Notify actor at end of protocol to save state

        Ok(())
    }

    async fn handle(&mut self, Complete { settlement }: Complete) -> Result<()> {
        let settlement = self
            .executor
            .execute(order_id, |cfd| {
                cfd.sign_collaborative_settlement_maker(proposal, dialer_signature)
            })
            .await?;
    }

    async fn handle(&mut self, msg: Reject) -> Result<()> {
        let Reject { order_id } = msg;

        let (mut framed, _) = self
            .pending_protocols
            .remove(&order_id)
            .with_context(|| format!("No active protocol for order {order_id}"))?;

        self.executor
            .execute(order_id, |cfd| {
                Ok(cfd.reject_collaborative_settlement(anyhow!("maker decision")))
            })
            .await?;

        self.tasks
            .add_fallible(framed.send(ListenerMessage::Msg1(Msg1::Reject)), |e| async move {
                tracing::debug!(%order_id, "Failed to reject collaborative settlement")
            });

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
    settlement: CollaborativeSettlement,
}

#[derive(Debug)]
enum Failed {
    BeforeReceiving {
        source: anyhow::Error,
    },
    AfterReceiving {
        transaction: ClosePositionTransaction,
        source: anyhow::Error,
    },
}

// Allows for easy use of `?`.
impl From<anyhow::Error> for Failed {
    fn from(source: anyhow::Error) -> Self {
        Failed::BeforeReceiving { source }
    }
}
