use crate::command;
use crate::oracle;
use crate::rollover;
use crate::rollover::protocol::Confirm;
use crate::rollover::protocol::Decision;
use crate::rollover::protocol::DialerError;
use crate::rollover::protocol::DialerMessage;
use crate::rollover::protocol::ListenerMessage;
use crate::rollover::protocol::Propose;
use crate::setup_contract;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
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
    pub async fn handle(
        &mut self,
        msg: ProposeRollover,
        ctx: &mut xtra::Context<Self>,
    ) -> Result<()> {
        let this = ctx.address().expect("we are alive");

        let ProposeRollover {
            order_id,
            maker_peer_id,
        } = msg;

        self.executor
            .execute(order_id, |cfd| cfd.start_rollover())
            .await?;

        let substream = self
            .endpoint
            .send(OpenSubstream::single_protocol(
                maker_peer_id.inner(),
                rollover::PROTOCOL,
            ))
            .await
            .context("Endpoint is disconnected")?
            .context("Failed to open substream")?;
        let mut framed = asynchronous_codec::Framed::new(
            substream,
            asynchronous_codec::JsonCodec::<DialerMessage, ListenerMessage>::new(),
        );

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
                let (rollover_params, dlc, position) = self
                    .executor
                    .execute(order_id, |cfd| {
                        cfd.handle_rollover_accepted_taker(tx_fee_rate, funding_rate)
                    })
                    .await?;

                let announcement = self
                    .get_announcement
                    .send(oracle::GetAnnouncement(oracle_event_id))
                    .await
                    .context("Oracle actor disconnected")?
                    .context("Failed to get announcement")?;

                tracing::info!(%order_id, "Rollover proposal got accepted");

                let funding_fee = *rollover_params.funding_fee();

                let (sink, stream) = framed.split();

                let rollover_fut = setup_contract::roll_over(
                    sink.with(|msg| future::ok(DialerMessage::RolloverMsg(msg))),
                    Box::pin(stream.filter_map(|msg| async move {
                        if let Ok(msg) = msg {
                            msg.into_rollover_msg().ok()
                        } else {
                            None
                        }
                    }))
                    .fuse(),
                    (self.oracle_pk, announcement),
                    rollover_params,
                    Role::Taker,
                    position,
                    dlc,
                    self.n_payouts,
                    complete_fee.into(),
                );

                let this = ctx.address().expect("self to be alive");
                self.tasks.add(async move {
                    // Use an explicit type annotation to cause a compile error if someone changes
                    // the handler.
                    let _: Result<(), xtra::Error> =
                        match rollover_fut.await.context("Rollover protocol failed") {
                            Ok(dlc) => todo!("Handle success"),
                            // this.send(RolloverSucceeded { dlc, funding_fee }).await,
                            Err(error) => todo!("Handle failure"), /* this.send(RolloverFailed {
                                                                    * error }).await, */
                        };
                });
            }
            Decision::Reject(_) => todo!("Handle the failed path"),
        }

        Ok(())
    }
}
