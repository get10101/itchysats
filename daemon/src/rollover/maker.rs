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
use model::CollaborativeSettlement;
use model::Dlc;
use model::FundingFee;
use model::FundingRate;
use model::OrderId;
use model::Position;
use model::TxFeeRate;
use std::collections::HashMap;
use tokio_tasks::Tasks;
use xtra_libp2p::NewInboundSubstream;
use xtra_libp2p::Substream;
use xtra_productivity::xtra_productivity;

use crate::command;
use crate::rollover::protocol::*;

use super::protocol;

/// Permanent actor to handle incoming substreams for the `/itchysats/rollover/1.0.0`
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
            // TODO: might need a bit more here
            PeerId,
        ),
    >,
    executor: command::Executor,
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

        self.tasks.add_fallible(
            async move {
                let mut framed =
                    Framed::new(stream, JsonCodec::<ListenerMessage, DialerMessage>::new());

                let propose = framed
                    .next()
                    .await
                    .context("End of stream while receiving Propose")?
                    .context("Failed to decode Propose")?
                    .into_propose()?;

                address
                    .send(ProposeReceived {
                        propose,
                        framed,
                        peer,
                    })
                    .await?;

                anyhow::Ok(())
            },
            |e| async move { tracing::warn!("Failed to handle incoming close position: {e:#}") },
        );
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: ProposeReceived) {
        let ProposeReceived {
            propose,
            framed,
            peer,
        } = msg;
        let order_id = propose.order_id;

        let result = self
            .executor
            .execute(order_id, |cfd| {
                // TODO: Validate that the correct peer is invoking this?
                cfd.start_rollover()
            })
            .await
            .context("Failed to start rollover protocol");

        if let Err(e) = result {
            tracing::debug!(%order_id, %peer, "Failed to start rollover protocol: {e:#}");
            return;
        }

        self.pending_protocols.insert(order_id, (framed, peer));
    }

    async fn handle(&mut self, msg: Accept, ctx: &mut xtra::Context<Self>) -> Result<()> {
        let Accept {
            order_id,
            tx_fee_rate,
            long_funding_rate,
            short_funding_rate,
        } = msg;
        let address = ctx.address().expect("we are alive");

        let (mut framed, peer) = self
            .pending_protocols
            .remove(&order_id)
            .with_context(|| format!("No active protocol for order {order_id}"))?;

        self.executor
            .execute(order_id, |cfd| {
                let funding_rate = match cfd.position() {
                    Position::Long => long_funding_rate,
                    Position::Short => short_funding_rate,
                };

                cfd.accept_rollover_proposal(tx_fee_rate, funding_rate, model::RolloverVersion::V2)
            })
            .await?;

        // self.tasks.add_fallible(
        //     async move {
        //         framed
        //             .send(ListenerMessage::Msg1(Msg1::Accept))
        //             .await
        //             .context("Failed to send Msg1::Accept")?;

        //         Ok(())
        //     },

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
            dlc,
            funding_fee,
        }: Complete,
    ) -> Result<()> {
        self.executor
            .execute(order_id, |cfd| Ok(cfd.complete_rollover(dlc, funding_fee)))
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
                Ok(cfd.reject_rollover(anyhow!("maker decision")))
            })
            .await?;

        self.tasks.add_fallible(
            async move {
                framed
                    .send(ListenerMessage::Decision(Decision::Reject(
                        protocol::Reject { order_id },
                    )))
                    .await
            },
            move |e| async move { tracing::debug!(%order_id, "Failed to reject rollover") },
        );

        Ok(())
    }
}

struct ProposeReceived {
    propose: Propose,
    framed: Framed<Substream, JsonCodec<ListenerMessage, DialerMessage>>,
    peer: PeerId,
}

/// Upon accepting Rollover maker sends the current estimated transaction fee and
/// funding rate
#[derive(Clone, Copy)]
pub struct Accept {
    pub order_id: OrderId,
    pub tx_fee_rate: TxFeeRate,
    pub long_funding_rate: FundingRate,
    pub short_funding_rate: FundingRate,
}

pub struct Reject {
    order_id: OrderId,
}

pub struct Complete {
    order_id: OrderId,
    dlc: Dlc,
    funding_fee: FundingFee,
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
