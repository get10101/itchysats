use crate::collab_settlement::protocol::*;
use crate::command;
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
use model::OrderId;
use model::SettlementProposal;
use model::SettlementTransaction;
use std::collections::HashMap;
use tokio_extras::FutureExt;
use xtra_libp2p::NewInboundSubstream;
use xtra_libp2p::Substream;
use xtra_productivity::xtra_productivity;

type ListenerConnection = (
    Framed<Substream, JsonCodec<ListenerMessage, DialerMessage>>,
    SettlementTransaction,
    SettlementProposal,
    PeerId,
);

/// Permanent actor to handle incoming substreams for the `/itchysats/collab-settlement/1.0.0`
/// protocol.
///
/// There is only one instance of this actor for all connections, meaning we must always spawn a
/// task whenever we interact with a substream to not block the execution of other connections.
pub struct Actor {
    pending_protocols: HashMap<OrderId, ListenerConnection>,
    executor: command::Executor,
    n_payouts: usize,
}

impl Actor {
    pub fn new(executor: command::Executor, n_payouts: usize) -> Self {
        Self {
            pending_protocols: HashMap::default(),
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

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: NewInboundSubstream, ctx: &mut xtra::Context<Self>) {
        let NewInboundSubstream { peer_id, stream } = msg;
        let address = ctx.address().expect("we are alive");

        tokio_extras::spawn_fallible(
            &address.clone(),
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
                        peer_id,
                    })
                    .await?;

                anyhow::Ok(())
            },
            move |e| async move {
                tracing::warn!(%peer_id, "Failed to handle incoming collab settlement: {e:#}")
            },
        );
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: ProposeReceived) {
        let ProposeReceived {
            propose,
            framed,
            peer_id,
        } = msg;
        let order_id = propose.id;

        let result = self
            .executor
            .execute(order_id, |cfd| {
                cfd.verify_counterparty_peer_id(&peer_id.into())?;
                cfd.start_collab_settlement_maker_olivia_max(
                    propose.price,
                    self.n_payouts,
                    &propose.unsigned_tx,
                )
            })
            .await
            .context("Failed to start collab settlement protocol");

        let (transaction, proposal) = match result {
            Ok((transaction, proposal)) => (transaction, proposal),
            Err(e) => {
                emit_failed(order_id, e, &self.executor).await;
                return;
            }
        };

        self.pending_protocols
            .insert(order_id, (framed, transaction, proposal, peer_id));
    }

    async fn handle(&mut self, msg: Accept, ctx: &mut xtra::Context<Self>) -> Result<()> {
        let Accept { order_id } = msg;

        let (mut framed, transaction, proposal, _peer) =
            self.pending_protocols
                .remove(&order_id)
                .with_context(|| format!("No active protocol for order {order_id}"))?;

        let this = ctx.address().expect("we are alive");
        tokio_extras::spawn_fallible(
            &this,
            {
                let executor = self.executor.clone();
                async move {
                    executor
                        .execute(order_id, |cfd| {
                            cfd.accept_collaborative_settlement_proposal(&proposal)
                        })
                        .await?;

                    framed
                        .send(ListenerMessage::Decision(Decision::Accept))
                        .await
                        .context("Failed to send Decision::Accept")?;

                    let DialerSignature { dialer_signature } = framed
                        .next()
                        .timeout(SETTLEMENT_MSG_TIMEOUT, || {
                            tracing::debug_span!("receive dialer signature")
                        })
                        .await
                        .with_context(|| {
                            format!(
                                "Taker did not send his signature within {} seconds.",
                                SETTLEMENT_MSG_TIMEOUT.as_secs()
                            )
                        })?
                        .context("End of stream while receiving DialerSignature")?
                        .context("Failed to decode DialerSignature")?
                        .into_dialer_signature()?;

                    let listener_signature = transaction.own_signature();

                    let settlement = transaction
                        .recv_counterparty_signature(dialer_signature)
                        .context("Failed to receive counterparty signature")?
                        .finalize()
                        .context("Failed to finalize transaction")?;

                    tracing::trace!(
                        ?settlement,
                        "Received collab settlement transaction from taker"
                    );

                    framed
                        .send(ListenerMessage::ListenerSignature(ListenerSignature {
                            listener_signature,
                        }))
                        .await
                        .map_err(|source| Failed::AfterReceiving {
                            source: anyhow!(source),
                            settlement: settlement.clone(),
                        })?;

                    emit_completed(order_id, settlement, &executor).await;
                    Ok(())
                }
            },
            {
                let executor = self.executor.clone();
                move |failed| async move {
                    match failed {
                        e @ Failed::BeforeReceiving { .. } => {
                            emit_failed(order_id, anyhow!(e), &executor).await;
                        }
                        e @ Failed::AfterReceiving { .. } => {
                            // TODO: proceed with the transaction when taker will be able to handle
                            // that case.
                            emit_failed(order_id, anyhow!(e), &executor).await;
                        }
                    }
                }
            },
        );

        Ok(())
    }

    async fn handle(&mut self, msg: Reject, ctx: &mut xtra::Context<Self>) -> Result<()> {
        let Reject { order_id } = msg;

        let (mut framed, ..) = self
            .pending_protocols
            .remove(&order_id)
            .with_context(|| format!("No active protocol for order {order_id}"))?;
        emit_rejected(order_id, &self.executor).await;

        let this = ctx.address().expect("we are alive");
        tokio_extras::spawn_fallible(
            &this,
            async move {
                framed
                    .send(ListenerMessage::Decision(Decision::Reject))
                    .await
            },
            move |e| async move {
                tracing::warn!(%order_id, "Failed to reject collaborative settlement: {e:#}")
            },
        );

        Ok(())
    }
}

struct ProposeReceived {
    propose: Propose,
    framed: Framed<Substream, JsonCodec<ListenerMessage, DialerMessage>>,
    peer_id: PeerId,
}

#[derive(Clone, Copy)]
pub struct Accept {
    pub order_id: OrderId,
}

#[derive(Clone, Copy)]
pub struct Reject {
    pub order_id: OrderId,
}

#[derive(Debug, thiserror::Error)]
enum Failed {
    #[error("Before receiving counterparty signature")]
    BeforeReceiving {
        #[from]
        source: Error,
    },
    #[error("After receiving counterparty signature")]
    AfterReceiving {
        settlement: CollaborativeSettlement,
        source: Error,
    },
}
