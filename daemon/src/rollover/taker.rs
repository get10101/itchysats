use crate::command;
use crate::future_ext::FutureExt;
use crate::oracle;
use crate::rollover;
use crate::rollover::protocol::emit_completed;
use crate::rollover::protocol::emit_failed;
use crate::rollover::protocol::emit_rejected;
use crate::rollover::protocol::Confirm;
use crate::rollover::protocol::Decision;
use crate::rollover::protocol::DialerMessage;
use crate::rollover::protocol::ListenerMessage;
use crate::rollover::protocol::Propose;
use crate::setup_contract;
use anyhow::Context;
use async_trait::async_trait;
use futures::future;
use futures::SinkExt;
use futures::StreamExt;
use maia_core::secp256k1_zkp::schnorrsig;
use model::libp2p::PeerId;
use model::OrderId;
use model::Role;
use model::Timestamp;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::message_channel::MessageChannel;
use xtra::Address;
use xtra_libp2p::Endpoint;
use xtra_libp2p::OpenSubstream;
use xtra_libp2p::Substream;
use xtra_productivity::xtra_productivity;

/// The duration that the taker waits until a decision (accept/reject) is expected from the maker
///
/// If the maker does not respond within `DECISION_TIMEOUT` seconds then the taker will fail the
/// rollover.
const DECISION_TIMEOUT: Duration = Duration::from_secs(30);

/// One actor to rule all the rollovers
pub struct Actor {
    endpoint: Address<Endpoint>,
    oracle_pk: schnorrsig::PublicKey,
    get_announcement: Box<dyn MessageChannel<oracle::GetAnnouncement>>,
    n_payouts: usize,
    tasks: Tasks,
    executor: command::Executor,
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

#[derive(Copy, Clone)]
pub struct ProposeRollover {
    pub order_id: OrderId,
    pub maker_peer_id: PeerId,
}

impl Actor {
    pub fn new(
        endpoint: Address<Endpoint>,
        executor: command::Executor,
        oracle_pk: schnorrsig::PublicKey,
        get_announcement: Box<dyn MessageChannel<oracle::GetAnnouncement>>,
        n_payouts: usize,
    ) -> Self {
        Self {
            endpoint,
            tasks: Tasks::default(),
            executor,
            get_announcement,
            oracle_pk,
            n_payouts,
        }
    }
}

impl Actor {
    async fn open_substream(&self, peer_id: PeerId) -> anyhow::Result<Substream> {
        Ok(self
            .endpoint
            .send(OpenSubstream::single_protocol(
                peer_id.inner(),
                rollover::PROTOCOL,
            ))
            .await
            .context("Endpoint is disconnected")
            .context("Failed to open substream")??)
    }
}

#[xtra_productivity]
impl Actor {
    pub async fn handle(&mut self, msg: ProposeRollover) {
        let ProposeRollover {
            order_id,
            maker_peer_id,
        } = msg;

        let substream = match self.open_substream(maker_peer_id).await {
            Ok(substream) => substream,
            Err(e) => {
                tracing::error!(%order_id, "Failed to start rollover: {e:#}");
                emit_failed(order_id, e, &self.executor).await;
                return;
            }
        };

        self.tasks.add_fallible(
            {
                let executor = self.executor.clone();
                let get_announcement = self.get_announcement.clone_channel();
                let oracle_pk = self.oracle_pk;
                let n_payouts = self.n_payouts;
                async move {
                    let mut framed = asynchronous_codec::Framed::new(
                        substream,
                        asynchronous_codec::JsonCodec::<DialerMessage, ListenerMessage>::new(),
                    );

                    executor
                        .execute(order_id, |cfd| cfd.start_rollover())
                        .await?;

                    framed
                        .send(DialerMessage::Propose(Propose {
                            order_id,
                            timestamp: Timestamp::now(),
                        }))
                        .await
                        .context("Failed to send Msg0")?;

                    match framed
                        .next()
                        .timeout(DECISION_TIMEOUT)
                        .await
                        .with_context(|| {
                            format!(
                                "Maker did not accept/reject within {} seconds.",
                                DECISION_TIMEOUT.as_secs()
                            )
                        })?
                        .context("End of stream while receiving rollover decision from maker")?
                        .context("Failed to decode rollover decision from maker")?
                        .into_decision()?
                    {
                        Decision::Confirm(Confirm {
                            order_id,
                            oracle_event_id,
                            tx_fee_rate,
                            funding_rate,
                            complete_fee,
                        }) => {
                            let (rollover_params, dlc, position) = executor
                                .execute(order_id, |cfd| {
                                    cfd.handle_rollover_accepted_taker(tx_fee_rate, funding_rate)
                                })
                                .await?;

                            let announcement = get_announcement
                                .send(oracle::GetAnnouncement(oracle_event_id))
                                .await
                                .context("Oracle actor disconnected")?
                                .context("Failed to get announcement")?;

                            tracing::info!(%order_id, "Rollover proposal got accepted");

                            let funding_fee = *rollover_params.funding_fee();

                            let (sink, stream) = framed.split();

                            let dlc = setup_contract::roll_over(
                                sink.with(|msg| {
                                    future::ok(DialerMessage::RolloverMsg(Box::new(msg)))
                                }),
                                Box::pin(stream.filter_map(|msg| async move {
                                    match msg {
                                        Ok(msg) => msg.into_rollover_msg().ok(),
                                        Err(e) => {
                                            tracing::error!(
                                                "Failed to convert rollover message: {e:#}"
                                            );
                                            None
                                        }
                                    }
                                }))
                                .fuse(),
                                (oracle_pk, announcement),
                                rollover_params,
                                Role::Taker,
                                position,
                                dlc,
                                n_payouts,
                                complete_fee.into(),
                            )
                            .await?;

                            emit_completed(order_id, dlc, funding_fee, &executor).await;
                        }
                        Decision::Reject(_) => {
                            emit_rejected(order_id, &executor).await;
                        }
                    }
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
    }
}
