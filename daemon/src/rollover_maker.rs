use crate::address_map::ActorName;
use crate::maker_inc_connections;
use crate::maker_inc_connections::TakerMessage;
use crate::model::cfd::Dlc;
use crate::model::cfd::OrderId;
use crate::model::cfd::Role;
use crate::model::cfd::RolloverProposal;
use crate::model::cfd::SettlementKind;
use crate::model::Identity;
use crate::oracle;
use crate::oracle::GetAnnouncement;
use crate::projection;
use crate::projection::UpdateRollOverProposal;
use crate::schnorrsig;
use crate::setup_contract;
use crate::tokio_ext::spawn_fallible;
use crate::wire;
use crate::wire::MakerToTaker;
use crate::wire::RollOverMsg;
use crate::xtra_ext::LogFailure;
use crate::Cfd;
use crate::Stopping;
use anyhow::Context as _;
use anyhow::Result;
use futures::channel::mpsc;
use futures::channel::mpsc::UnboundedSender;
use futures::future;
use futures::SinkExt;
use xtra::prelude::MessageChannel;
use xtra::Context;
use xtra::KeepRunning;
use xtra_productivity::xtra_productivity;

pub struct AcceptRollOver;

pub struct RejectRollOver;

pub struct ProtocolMsg(pub wire::RollOverMsg);

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

#[allow(clippy::large_enum_variant)]
pub struct Completed {
    pub order_id: OrderId,
    pub dlc: Dlc,
}

pub struct Actor {
    send_to_taker_actor: Box<dyn MessageChannel<TakerMessage>>,
    cfd: Cfd,
    taker_id: Identity,
    n_payouts: usize,
    oracle_pk: schnorrsig::PublicKey,
    sent_from_taker: Option<UnboundedSender<RollOverMsg>>,
    on_completed: Box<dyn MessageChannel<Completed>>,
    oracle_actor: Box<dyn MessageChannel<GetAnnouncement>>,
    on_stopping: Vec<Box<dyn MessageChannel<Stopping<Self>>>>,
    projection_actor: xtra::Address<projection::Actor>,
    proposal: RolloverProposal,
}

#[async_trait::async_trait]
impl xtra::Actor for Actor {
    async fn stopping(&mut self, ctx: &mut Context<Self>) -> KeepRunning {
        let address = ctx.address().expect("acquired own actor address");

        for channel in self.on_stopping.iter() {
            let _ = channel
                .send(Stopping {
                    me: address.clone(),
                })
                .await;
        }

        KeepRunning::StopAll
    }

    async fn started(&mut self, _ctx: &mut Context<Self>) {
        let _ = self
            .update_proposal(Some((self.proposal.clone(), SettlementKind::Incoming)))
            .await;
    }

    async fn stopped(self) {
        let _ = self.update_proposal(None).await;
    }
}

impl Actor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        send_to_taker_actor: &(impl MessageChannel<TakerMessage> + 'static),
        cfd: Cfd,
        taker_id: Identity,
        oracle_pk: schnorrsig::PublicKey,
        on_completed: &(impl MessageChannel<Completed> + 'static),
        oracle_actor: &(impl MessageChannel<GetAnnouncement> + 'static),
        (on_stopping0, on_stopping1): (
            &(impl MessageChannel<Stopping<Self>> + 'static),
            &(impl MessageChannel<Stopping<Self>> + 'static),
        ),
        projection_actor: xtra::Address<projection::Actor>,
        proposal: RolloverProposal,
        n_payouts: usize,
    ) -> Self {
        Self {
            send_to_taker_actor: send_to_taker_actor.clone_channel(),
            cfd,
            taker_id,
            n_payouts,
            oracle_pk,
            sent_from_taker: None,
            on_completed: on_completed.clone_channel(),
            oracle_actor: oracle_actor.clone_channel(),
            on_stopping: vec![on_stopping0.clone_channel(), on_stopping1.clone_channel()],
            projection_actor,
            proposal,
        }
    }

    async fn update_proposal(
        &self,
        proposal: Option<(RolloverProposal, SettlementKind)>,
    ) -> Result<()> {
        self.projection_actor
            .send(UpdateRollOverProposal {
                order: self.cfd.id(),
                proposal,
            })
            .await?;

        Ok(())
    }

    async fn fail(&mut self, ctx: &mut xtra::Context<Self>, error: anyhow::Error) {
        tracing::info!(id = %self.cfd.id(), %error, "Rollover failed");

        ctx.stop();
    }

    async fn accept(&mut self, ctx: &mut xtra::Context<Self>) -> Result<()> {
        let order_id = self.cfd.id();

        if self.sent_from_taker.is_some() {
            tracing::warn!(%order_id, "Rollover already active");
            return Ok(());
        }

        let (sender, receiver) = mpsc::unbounded();

        self.sent_from_taker = Some(sender);

        tracing::debug!(%order_id, "Maker accepts a roll_over proposal" );

        let (rollover_params, dlc, interval) = self.cfd.start_rollover()?;

        let oracle_event_id =
            oracle::next_announcement_after(time::OffsetDateTime::now_utc() + interval)?;

        let taker_id = self.taker_id;

        self.send_to_taker_actor
            .send(maker_inc_connections::TakerMessage {
                taker_id,
                msg: wire::MakerToTaker::ConfirmRollOver {
                    order_id,
                    oracle_event_id,
                },
            })
            .await??;

        let _ = self.update_proposal(None).await;

        let announcement = self
            .oracle_actor
            .send(oracle::GetAnnouncement(oracle_event_id))
            .await?
            .with_context(|| format!("Announcement {} not found", oracle_event_id))?;

        let rollover_fut = setup_contract::roll_over(
            self.send_to_taker_actor.sink().with(move |msg| {
                future::ok(maker_inc_connections::TakerMessage {
                    taker_id,
                    msg: wire::MakerToTaker::RollOverProtocol { order_id, msg },
                })
            }),
            receiver,
            (self.oracle_pk, announcement),
            rollover_params,
            Role::Maker,
            dlc,
            self.n_payouts,
        );

        let this = ctx.address().expect("self to be alive");

        spawn_fallible::<_, anyhow::Error>(async move {
            let _ = match rollover_fut.await {
                Ok(dlc) => this.send(RolloverSucceeded { dlc }).await?,
                Err(error) => this.send(RolloverFailed { error }).await?,
            };

            Ok(())
        });

        Ok(())
    }

    async fn reject(&mut self, ctx: &mut xtra::Context<Self>) -> Result<()> {
        tracing::info!(id = %self.cfd.id(), "Maker rejects a roll_over proposal" );

        self.send_to_taker_actor
            .send(TakerMessage {
                taker_id: self.taker_id,
                msg: MakerToTaker::RejectRollOver(self.cfd.id()),
            })
            .await??;

        ctx.stop();

        Ok(())
    }

    pub async fn forward_protocol_msg(&mut self, msg: ProtocolMsg) -> Result<()> {
        let sender = self
            .sent_from_taker
            .as_mut()
            .context("cannot forward message to rollover task")?;
        sender.send(msg.0).await?;
        Ok(())
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle_accept_rollover(
        &mut self,
        _msg: AcceptRollOver,
        ctx: &mut xtra::Context<Self>,
    ) {
        if let Err(err) = self.accept(ctx).await {
            self.fail(ctx, err).await;
        };
    }

    async fn handle_reject_rollover(
        &mut self,
        _msg: RejectRollOver,
        ctx: &mut xtra::Context<Self>,
    ) {
        if let Err(err) = self.reject(ctx).await {
            self.fail(ctx, err).await;
        };
    }

    async fn handle_protocol_msg(&mut self, msg: ProtocolMsg, ctx: &mut xtra::Context<Self>) {
        if let Err(err) = self.forward_protocol_msg(msg).await {
            self.fail(ctx, err).await;
        };
    }

    async fn handle_rollover_failed(&mut self, msg: RolloverFailed, ctx: &mut xtra::Context<Self>) {
        self.fail(ctx, msg.error).await;
    }

    async fn handle_rollover_succeeded(
        &mut self,
        msg: RolloverSucceeded,
        ctx: &mut xtra::Context<Self>,
    ) {
        let _: Result<(), xtra::Disconnected> = self
            .on_completed
            .send(Completed {
                order_id: self.cfd.id(),
                dlc: msg.dlc,
            })
            .log_failure("Failed to report rollover completion")
            .await;

        ctx.stop();
    }
}

impl ActorName for Actor {
    fn actor_name() -> String {
        "Maker rollover".to_string()
    }
}
