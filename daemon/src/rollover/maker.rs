use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use asynchronous_codec::Framed;
use asynchronous_codec::JsonCodec;
use futures::future;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::PeerId;
use maia_core::secp256k1_zkp::schnorrsig;
use model::FundingRate;
use model::OrderId;
use model::Position;
use model::Role;
use model::RolloverVersion;
use model::TxFeeRate;
use std::collections::HashMap;
use tokio_tasks::Tasks;
use xtra::message_channel::MessageChannel;
use xtra_libp2p::NewInboundSubstream;
use xtra_libp2p::Substream;
use xtra_productivity::xtra_productivity;

use crate::command;
use crate::oracle;
use crate::rollover::protocol::*;
use crate::setup_contract;

use super::protocol;

type ListenerConnection = (
    Framed<Substream, JsonCodec<ListenerMessage, DialerMessage>>,
    PeerId,
);

/// Permanent actor to handle incoming substreams for the `/itchysats/rollover/1.0.0`
/// protocol.
///
/// There is only one instance of this actor for all connections, meaning we must always spawn a
/// task whenever we interact with a substream to not block the execution of other connections.
pub struct Actor {
    tasks: Tasks,
    oracle_pk: schnorrsig::PublicKey,
    get_announcement: Box<dyn MessageChannel<oracle::GetAnnouncement>>,
    n_payouts: usize,
    pending_protocols: HashMap<OrderId, ListenerConnection>,
    executor: command::Executor,
}

impl Actor {
    pub fn new(
        executor: command::Executor,
        oracle_pk: schnorrsig::PublicKey,
        get_announcement: Box<dyn MessageChannel<oracle::GetAnnouncement>>,
        n_payouts: usize,
    ) -> Self {
        Self {
            tasks: Tasks::default(),
            oracle_pk,
            get_announcement,
            n_payouts,
            pending_protocols: HashMap::default(),
            executor,
        }
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle(&mut self, msg: NewInboundSubstream, ctx: &mut xtra::Context<Self>) {
        let NewInboundSubstream { peer, stream } = msg;
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
            |e| async move { tracing::warn!("Failed to handle incoming rollover protocol: {e:#}") },
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

        if let Err(e) = self
            .executor
            .execute(order_id, |cfd| {
                // TODO: Validate that the correct peer is invoking this?
                cfd.start_rollover()
            })
            .await
        {
            tracing::warn!(%order_id, "Rollover failed after handling taker proposal: {e:#}");

            // We have to append failed to ensure that we can rollover in the future
            // The cfd logic might otherwise prevent us from starting a rollover if there is still
            // one ongoing that was not properly ended.
            emit_failed(order_id, e, &self.executor).await;

            return;
        }

        // In case we fail to accept/reject some proposals might never get cleaned up. Given that
        // the taker will retry this should not do much harm because we will replace the proposal
        // with a new one. This is acceptable for now.
        self.pending_protocols.insert(order_id, (framed, peer));
    }

    async fn handle(&mut self, msg: Accept) -> Result<()> {
        let Accept {
            order_id,
            tx_fee_rate,
            long_funding_rate,
            short_funding_rate,
        } = msg;

        let (mut framed, _) = self.pending_protocols.remove(&order_id).with_context(|| {
            format!("No active protocol for {order_id} when accepting rollover")
        })?;

        self.tasks.add_fallible(
            {
                let executor = self.executor.clone();
                let get_announcement = self.get_announcement.clone_channel();
                let oracle_pk = self.oracle_pk;
                let n_payouts = self.n_payouts;
                async move {
                    let (rollover_params, dlc, position, oracle_event_id, funding_rate) = executor
                        .execute(order_id, |cfd| {
                            let funding_rate = match cfd.position() {
                                Position::Long => long_funding_rate,
                                Position::Short => short_funding_rate,
                            };

                            let (event, params, dlc, position, oracle_event_id) = cfd
                                .accept_rollover_proposal(
                                    tx_fee_rate,
                                    funding_rate,
                                    RolloverVersion::V3,
                                )?;

                            Ok((event, params, dlc, position, oracle_event_id, funding_rate))
                        })
                        .await?;

                    let complete_fee = rollover_params
                        .fee_account
                        .add_funding_fee(rollover_params.current_fee)
                        .settle();

                    framed
                        .send(ListenerMessage::Decision(Decision::Confirm(Confirm {
                            order_id,
                            oracle_event_id,
                            tx_fee_rate,
                            funding_rate,
                            complete_fee: complete_fee.into(),
                        })))
                        .await
                        .context("Failed to send rollover confirmation message")?;

                    let announcement = get_announcement
                        .send(oracle::GetAnnouncement(oracle_event_id))
                        .await
                        .context("Oracle actor disconnected")?
                        .context("Failed to get announcement")?;

                    let funding_fee = *rollover_params.funding_fee();

                    let (sink, stream) = framed.split();

                    let dlc = setup_contract::roll_over(
                        sink.with(|msg| future::ok(ListenerMessage::RolloverMsg(Box::new(msg)))),
                        Box::pin(stream.filter_map(|msg| async move {
                            match msg {
                                Ok(msg) => msg.into_rollover_msg().ok(),
                                Err(e) => {
                                    tracing::error!("Failed to convert rollover message: {e:#}");
                                    None
                                }
                            }
                        }))
                        .fuse(),
                        (oracle_pk, announcement),
                        rollover_params,
                        Role::Maker,
                        position,
                        dlc,
                        n_payouts,
                        complete_fee,
                    )
                    .await?;

                    emit_completed(order_id, dlc, funding_fee, &executor).await;

                    Ok(())
                }
            },
            {
                let executor = self.executor.clone();
                move |e| async move {
                    emit_failed(order_id, e, &executor).await;
                }
            },
        );

        Ok(())
    }

    async fn handle(&mut self, msg: Reject) -> Result<()> {
        let Reject { order_id } = msg;

        let (mut framed, ..) = self.pending_protocols.remove(&order_id).with_context(|| {
            format!("No active protocol for {order_id} when rejecting rollover")
        })?;

        emit_rejected(order_id, &self.executor).await;

        self.tasks.add_fallible(
            async move {
                framed
                    .send(ListenerMessage::Decision(Decision::Reject(
                        protocol::Reject { order_id },
                    )))
                    .await
            },
            move |e| async move {
                tracing::debug!(%order_id, "Failed to send reject rollover to the taker: {e:#}")
            },
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
#[derive(Clone, Copy, Debug)]
pub struct Accept {
    pub order_id: OrderId,
    pub tx_fee_rate: TxFeeRate,
    pub long_funding_rate: FundingRate,
    pub short_funding_rate: FundingRate,
}

#[derive(Clone, Copy, Debug)]
pub struct Reject {
    pub order_id: OrderId,
}