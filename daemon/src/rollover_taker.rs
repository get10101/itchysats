use crate::address_map::Stopping;
use crate::command;
use crate::connection;
use crate::model::cfd::Dlc;
use crate::model::cfd::OrderId;
use crate::model::cfd::Role;
use crate::model::cfd::RolloverCompleted;
use crate::model::BitMexPriceEventId;
use crate::model::FundingFee;
use crate::model::FundingRate;
use crate::model::Timestamp;
use crate::model::TxFeeRate;
use crate::oracle;
use crate::oracle::GetAnnouncement;
use crate::process_manager;
use crate::setup_contract;
use crate::wire;
use crate::wire::RolloverMsg;
use crate::Tasks;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use futures::channel::mpsc;
use futures::channel::mpsc::UnboundedSender;
use futures::future;
use futures::SinkExt;
use maia::secp256k1_zkp::schnorrsig;
use std::time::Duration;
use xtra::prelude::MessageChannel;
use xtra::Disconnected;
use xtra_productivity::xtra_productivity;

/// The maximum amount of time we give the maker to send us a response.
const MAKER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Actor {
    id: OrderId,
    n_payouts: usize,
    oracle_pk: schnorrsig::PublicKey,
    maker: xtra::Address<connection::Actor>,
    get_announcement: Box<dyn MessageChannel<GetAnnouncement>>,
    on_stopping: Vec<Box<dyn MessageChannel<Stopping<Self>>>>,
    rollover_msg_sender: Option<UnboundedSender<RolloverMsg>>,
    executor: command::Executor,
    tasks: Tasks,
}

impl Actor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: OrderId,
        n_payouts: usize,
        oracle_pk: schnorrsig::PublicKey,
        maker: xtra::Address<connection::Actor>,
        get_announcement: &(impl MessageChannel<GetAnnouncement> + 'static),
        process_manager: xtra::Address<process_manager::Actor>,
        (on_stopping0, on_stopping1): (
            &(impl MessageChannel<Stopping<Self>> + 'static),
            &(impl MessageChannel<Stopping<Self>> + 'static),
        ),
        db: sqlx::SqlitePool,
    ) -> Self {
        Self {
            id,
            n_payouts,
            oracle_pk,
            maker,
            get_announcement: get_announcement.clone_channel(),
            on_stopping: vec![on_stopping0.clone_channel(), on_stopping1.clone_channel()],
            rollover_msg_sender: None,
            tasks: Tasks::default(),
            executor: command::Executor::new(db, process_manager),
        }
    }

    async fn propose(&mut self, this: xtra::Address<Self>) -> Result<()> {
        self.executor
            .execute(self.id, |cfd| cfd.start_rollover())
            .await?;

        tracing::trace!(order_id=%self.id, "Proposing rollover");

        self.maker
            .send(connection::ProposeRollover {
                order_id: self.id,
                timestamp: Timestamp::now(),
                address: this,
            })
            .await
            .context("Failed to propose rollover")??;

        Ok(())
    }

    async fn handle_confirmed(
        &mut self,
        msg: RolloverAccepted,
        ctx: &mut xtra::Context<Self>,
    ) -> Result<()> {
        let RolloverAccepted {
            oracle_event_id,
            tx_fee_rate,
            funding_rate,
        }: RolloverAccepted = msg;
        let order_id = self.id;

        let (rollover_params, dlc) = self
            .executor
            .execute(self.id, |cfd| {
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

        let (sender, receiver) = mpsc::unbounded::<RolloverMsg>();
        // store the writing end to forward messages from the maker to
        // the spawned rollover task
        self.rollover_msg_sender = Some(sender);

        let funding_fee = *rollover_params.funding_fee();

        let rollover_fut = setup_contract::roll_over(
            xtra::message_channel::MessageChannel::sink(&self.maker).with(move |msg| {
                future::ok(wire::TakerToMaker::RolloverProtocol { order_id, msg })
            }),
            receiver,
            (self.oracle_pk, announcement),
            rollover_params,
            Role::Taker,
            dlc,
            self.n_payouts,
        );

        let this = ctx.address().expect("self to be alive");
        self.tasks.add(async move {
            // Use an explicit type annotation to cause a compile error if someone changes the
            // handler.
            let _: Result<(), Disconnected> =
                match rollover_fut.await.context("Rollover protocol failed") {
                    Ok(dlc) => this.send(RolloverSucceeded { dlc, funding_fee }).await,
                    Err(error) => this.send(RolloverFailed { error }).await,
                };
        });

        Ok(())
    }

    async fn forward_protocol_msg(&mut self, msg: wire::RolloverMsg) -> Result<()> {
        self.rollover_msg_sender
            .as_mut()
            .context("Rollover task is not active")? // Sender is set once `Accepted` is received.
            .send(msg)
            .await
            .context("Failed to forward message to rollover task")?;

        Ok(())
    }

    async fn complete(&mut self, completed: RolloverCompleted, ctx: &mut xtra::Context<Self>) {
        let result = self
            .executor
            .execute(self.id, |cfd| Ok(cfd.roll_over(completed)?))
            .await;

        if let Err(e) = result {
            tracing::warn!(order_id = %self.id, "Failed to complete rollover: {:#}", e)
        }

        ctx.stop();
    }

    /// Returns whether the maker has accepted our rollover proposal.
    fn is_accepted(&self) -> bool {
        self.rollover_msg_sender.is_some()
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("self to be alive");

        if let Err(error) = self.propose(this).await {
            self.complete(
                RolloverCompleted::Failed {
                    order_id: self.id,
                    error,
                },
                ctx,
            )
            .await;

            return;
        }

        let maker_response_timeout = {
            let this = ctx.address().expect("self to be alive");
            async move {
                tokio::time::sleep(MAKER_RESPONSE_TIMEOUT).await;

                this.send(MakerResponseTimeoutReached {
                    timeout: MAKER_RESPONSE_TIMEOUT,
                })
                .await
                .expect("can send to ourselves");
            }
        };

        self.tasks.add(maker_response_timeout);
    }

    async fn stopping(&mut self, ctx: &mut xtra::Context<Self>) -> xtra::KeepRunning {
        // inform other actors that we are stopping so that our
        // address can be GCd from their AddressMaps
        let me = ctx.address().expect("we are still alive");

        for channel in self.on_stopping.iter() {
            let _ = channel.send(Stopping { me: me.clone() }).await;
        }

        xtra::KeepRunning::StopAll
    }
}

#[xtra_productivity]
impl Actor {
    pub async fn handle_confirm_rollover(
        &mut self,
        msg: RolloverAccepted,
        ctx: &mut xtra::Context<Self>,
    ) {
        if let Err(error) = self.handle_confirmed(msg, ctx).await {
            self.complete(
                RolloverCompleted::Failed {
                    order_id: self.id,
                    error,
                },
                ctx,
            )
            .await;
        }
    }

    pub async fn reject_rollover(&mut self, _: RolloverRejected, ctx: &mut xtra::Context<Self>) {
        let order_id = self.id;

        tracing::info!(%order_id, "Rollover proposal got rejected");

        self.complete(RolloverCompleted::rejected(order_id), ctx)
            .await;
    }

    pub async fn handle_rollover_succeeded(
        &mut self,
        msg: RolloverSucceeded,
        ctx: &mut xtra::Context<Self>,
    ) {
        self.complete(
            RolloverCompleted::succeeded(self.id, msg.dlc, msg.funding_fee),
            ctx,
        )
        .await;
    }

    pub async fn handle_rollover_failed(
        &mut self,
        msg: RolloverFailed,
        ctx: &mut xtra::Context<Self>,
    ) {
        self.complete(
            RolloverCompleted::Failed {
                order_id: self.id,
                error: msg.error,
            },
            ctx,
        )
        .await;
    }

    pub async fn handle_rollover_timeout_reached(
        &mut self,
        msg: MakerResponseTimeoutReached,
        ctx: &mut xtra::Context<Self>,
    ) {
        // If we are accepted, discard the timeout because the maker DID respond.
        if self.is_accepted() {
            return;
        }

        // Otherwise, fail because we did not receive a response.
        // If the proposal is rejected, our entire actor would already be shut down and we hence
        // never get this message.
        let timeout = msg.timeout.as_secs();
        let completed = RolloverCompleted::Failed {
            order_id: self.id,
            error: anyhow!("Maker did not respond within {timeout} seconds"),
        };

        self.complete(completed, ctx).await;
    }

    pub async fn handle_protocol_msg(
        &mut self,
        msg: wire::RolloverMsg,
        ctx: &mut xtra::Context<Self>,
    ) {
        if let Err(error) = self.forward_protocol_msg(msg).await {
            self.complete(
                RolloverCompleted::Failed {
                    order_id: self.id,
                    error,
                },
                ctx,
            )
            .await;
        }
    }
}

/// Message sent from the `connection::Actor` to the
/// `rollover_taker::Actor` to notify that the maker has accepted the
/// rollover proposal.
pub struct RolloverAccepted {
    pub oracle_event_id: BitMexPriceEventId,
    pub tx_fee_rate: TxFeeRate,
    pub funding_rate: FundingRate,
}

/// Message sent from the `connection::Actor` to the
/// `rollover_taker::Actor` to notify that the maker has rejected the
/// rollover proposal.
pub struct RolloverRejected;

/// Message sent from the spawned task to `rollover_taker::Actor` to
/// notify that rollover has finished successfully.
struct RolloverSucceeded {
    dlc: Dlc,
    funding_fee: FundingFee,
}

/// Message sent from the spawned task to `rollover_taker::Actor` to
/// notify that rollover has failed.
struct RolloverFailed {
    error: anyhow::Error,
}

/// Message sent from the spawned task to `rollover_taker::Actor` to
/// notify that the timeout has been reached.
///
/// It is up to the actor to reason whether or not the protocol has progressed since then.
struct MakerResponseTimeoutReached {
    timeout: Duration,
}
