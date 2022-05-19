use crate::command;
use crate::oracle;
use crate::rollover;
use crate::rollover::protocol::{Confirm, roll_over_maker, roll_over_taker};
use crate::rollover::protocol::Decision;
use crate::rollover::protocol::DialerMessage;
use crate::rollover::protocol::ListenerMessage;
use crate::rollover::protocol::Propose;
use crate::setup_contract;
use anyhow::anyhow;
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
use tokio_tasks::Tasks;
use xtra::message_channel::MessageChannel;
use xtra::Address;
use xtra_libp2p::Endpoint;
use xtra_libp2p::OpenSubstream;
use xtra_productivity::xtra_productivity;

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

#[xtra_productivity]
impl Actor {
    pub async fn handle(&mut self, msg: ProposeRollover) {
        let ProposeRollover {
            order_id,
            maker_peer_id,
        } = msg;

        let substream = match self
            .endpoint
            .send(OpenSubstream::single_protocol(
                maker_peer_id.inner(),
                rollover::PROTOCOL,
            ))
            .await
            .context("Endpoint is disconnected")
            .context("Failed to open substream")
        {
            Ok(Ok(substream)) => substream,
            // TODO: Same error path and add failed
            Ok(Err(e)) => {
                tracing::error!("Failed to open connection to maker: {e:#}");
                return;
            }
            Err(e) => {
                tracing::error!(%order_id, "Failed to open connection to maker for  rollover: {e:#}");

                if let Err(e) = self
                    .executor
                    .execute(order_id, |cfd| Ok(cfd.fail_rollover(e)))
                    .await
                {
                    tracing::error!(%order_id, "Failed to execute rollover failed: {e:#}")
                }

                return;
            }
        };

        self.tasks.add_fallible({
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

                // TODO: We will need to apply a timeout to these. Perhaps we can put a timeout generally
                // into "reading from the substream"?
                match framed
                    .next()
                    .await
                    .context("End of stream while receiving Msg1")?
                    .context("Failed to decode Msg1")?
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

                        // let (sink, stream) = framed.split();

                        let dlc = roll_over_taker(
                            framed,
                            (oracle_pk, announcement),
                            rollover_params,
                            Role::Taker,
                            position,
                            dlc,
                            n_payouts,
                            complete_fee.into(),
                        ).await?;

                        // TODO: For now we assume that we can always save into the db, we should re-try here
                        if let Err(e) = executor
                            .execute(order_id, |cfd| Ok(cfd.complete_rollover(dlc, funding_fee)))
                            .await
                        {
                            tracing::warn!(%order_id, "Failed to execute rollover completed: {e:#}")
                        }
                    }
                    Decision::Reject(_) => {
                        if let Err(e) = executor
                            .execute(order_id, |cfd| {
                                Ok(cfd.reject_rollover(anyhow!("Maker rejected rollover proposal")))
                            })
                            .await
                        {
                            tracing::error!(%order_id, "Failed to execute rollover rejected: {e:#}")
                        }
                    }
                }
                Ok(())
            } }, {
                let executor = self.executor.clone();
                move |e|                 async move {
                    tracing::error!(%order_id, "Rollover failed: {e:#}");

                    if let Err(e) = executor
                        .execute(order_id, |cfd| Ok(cfd.fail_rollover(e)))
                        .await
                    {
                        tracing::error!(%order_id, "Failed to execute rollover failed: {e:#}")
                    }
                }
            }
        );
    }
}
