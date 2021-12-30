use crate::address_map::ActorName;
use crate::address_map::Stopping;
use crate::cfd_actors::load_cfd;
use crate::connection;
use crate::model::cfd::Dlc;
use crate::model::cfd::OrderId;
use crate::model::cfd::Role;
use crate::model::cfd::RolloverCompleted;
use crate::model::cfd::RolloverError;
use crate::model::BitMexPriceEventId;
use crate::model::Timestamp;
use crate::oracle;
use crate::oracle::GetAnnouncement;
use crate::process_manager;
use crate::setup_contract;
use crate::setup_contract::RolloverParams;
use crate::wire;
use crate::wire::RollOverMsg;
use crate::Tasks;
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
    process_manager: xtra::Address<process_manager::Actor>,
    on_stopping: Vec<Box<dyn MessageChannel<Stopping<Self>>>>,
    rollover_msg_sender: Option<UnboundedSender<RollOverMsg>>,
    tasks: Tasks,

    // TODO: It's a weird indirection to have the params and dlc in here
    //  We do this, because it is "ephemeral" and we cannot easily retrieve it from the Cfd in the
    // context of a rollover.  We could potentially have a "current rollover" in the Cfd itself
    // that is set upon RolloverStarted. Not sure that's better.
    rollover_params: Option<RolloverParams>,
    dlc: Option<Dlc>,
    db: sqlx::SqlitePool,
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
            process_manager,
            on_stopping: vec![on_stopping0.clone_channel(), on_stopping1.clone_channel()],
            rollover_msg_sender: None,
            tasks: Tasks::default(),
            rollover_params: None,
            dlc: None,
            db,
        }
    }

    async fn propose(&mut self, this: xtra::Address<Self>) -> Result<(), RolloverError> {
        let mut conn = self.db.acquire().await.context("Failed to connect to DB")?;
        let cfd = load_cfd(self.id, &mut conn)
            .await
            .context("Failed to load CFD")?;

        let (event, (rollover_params, dlc)) = cfd.start_rollover_taker()?;

        self.process_manager
            .send(process_manager::Event::new(event.clone()))
            .await
            .context("Process manager actor disconnected")?
            .with_context(|| format!("Process manager failed to process event {:?}", event))?;

        self.rollover_params = Some(rollover_params);
        self.dlc = Some(dlc);

        tracing::trace!(order_id=%self.id, "Proposing rollover");
        self.maker
            .send(connection::ProposeRollOver {
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
        msg: RollOverAccepted,
        ctx: &mut xtra::Context<Self>,
    ) -> Result<(), RolloverError> {
        // TODO: Check the oracle event id for sanity (i.e. is in the future according to settlement
        // interval)
        let RollOverAccepted { oracle_event_id } = msg;
        let order_id = self.id;

        let mut conn = self.db.acquire().await.context("Failed to connect to DB")?;
        let cfd = load_cfd(self.id, &mut conn)
            .await
            .context("Failed to load CFD")?;

        let event = cfd.handle_rollover_accepted_taker()?;
        self.process_manager
            .send(process_manager::Event::new(event.clone()))
            .await
            .context("Process manager actor disconnected")?
            .with_context(|| format!("Process manager failed to process event {:?}", event))?;

        let announcement = self
            .get_announcement
            .send(oracle::GetAnnouncement(oracle_event_id))
            .await
            .context("Oracle actor disconnected")?
            .context("Failed to get announcement")?;

        tracing::info!(%order_id, "Rollover proposal got accepted");

        let (sender, receiver) = mpsc::unbounded::<RollOverMsg>();
        // store the writing end to forward messages from the maker to
        // the spawned rollover task
        self.rollover_msg_sender = Some(sender);
        let rollover_params = self
            .rollover_params
            .as_ref()
            .context("Rollover parameters not set upon completion - what is happening?!")?
            .clone();
        let dlc = self
            .dlc
            .as_ref()
            .context("Dlc not set upon completion - what is happening?!")?
            .clone();

        let rollover_fut = setup_contract::roll_over(
            xtra::message_channel::MessageChannel::sink(&self.maker).with(move |msg| {
                future::ok(wire::TakerToMaker::RollOverProtocol { order_id, msg })
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
            let _: Result<(), Disconnected> = match rollover_fut.await {
                Ok(dlc) => this.send(RolloverSucceeded { dlc }).await,
                Err(error) => this.send(RolloverFailed { error }).await,
            };
        });

        Ok(())
    }

    async fn forward_protocol_msg(&mut self, msg: wire::RollOverMsg) -> Result<(), RolloverError> {
        self.rollover_msg_sender
            .as_mut()
            .context("Rollover task is not active")? // Sender is set once `Accepted` is received.
            .send(msg)
            .await
            .context("Failed to forward message to rollover task")?;

        Ok(())
    }

    async fn complete(&mut self, completed: RolloverCompleted, ctx: &mut xtra::Context<Self>) {
        let order_id = self.id;
        let event_fut = async {
            let mut conn = self.db.acquire().await?;
            let cfd = load_cfd(order_id, &mut conn).await?;
            let event = cfd.roll_over(completed)?;

            anyhow::Ok(event)
        };

        match event_fut.await {
            Ok(event) => {
                if let Some(event) = event {
                    let _ = self
                        .process_manager
                        .send(process_manager::Event::new(event))
                        .await;
                }
            }
            Err(e) => {
                tracing::warn!(%order_id, "Failed to complete rollover: {:#}", e)
            }
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
        msg: RollOverAccepted,
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

    pub async fn reject_rollover(&mut self, _: RollOverRejected, ctx: &mut xtra::Context<Self>) {
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
        self.complete(RolloverCompleted::succeeded(self.id, msg.dlc), ctx)
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
                error: RolloverError::Protocol { source: msg.error },
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
        let completed = RolloverCompleted::Failed {
            order_id: self.id,
            error: RolloverError::MakerDidNotRespond {
                timeout: msg.timeout.as_secs(),
            },
        };

        self.complete(completed, ctx).await;
    }

    pub async fn handle_protocol_msg(
        &mut self,
        msg: wire::RollOverMsg,
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
pub struct RollOverAccepted {
    pub oracle_event_id: BitMexPriceEventId,
}

/// Message sent from the `connection::Actor` to the
/// `rollover_taker::Actor` to notify that the maker has rejected the
/// rollover proposal.
pub struct RollOverRejected;

/// Message sent from the spawned task to `rollover_taker::Actor` to
/// notify that rollover has finished successfully.
struct RolloverSucceeded {
    dlc: Dlc,
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

impl ActorName for Actor {
    fn actor_name() -> String {
        "Taker rollover".to_string()
    }
}
