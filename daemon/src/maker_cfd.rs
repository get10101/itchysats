use crate::address_map::{AddressMap, Stopping};
use crate::cfd_actors::{self, append_cfd_state, insert_cfd_and_update_feed};
use crate::db::{insert_order, load_cfd_by_order_id, load_order_by_id};
use crate::model::cfd::{
    Cfd, CfdState, CfdStateCommon, CollaborativeSettlement, Dlc, Order, OrderId, Origin, Role,
    RollOverProposal, SettlementKind, SettlementProposal, UpdateCfdProposal,
};
use crate::model::{Identity, Price, Timestamp, Usd};
use crate::monitor::MonitorParams;
use crate::projection::{
    try_into_update_rollover_proposal, try_into_update_settlement_proposal, Update,
    UpdateRollOverProposal, UpdateSettlementProposal,
};
use crate::setup_contract::RolloverParams;
use crate::tokio_ext::FutureExt;
use crate::wire::TakerToMaker;
use crate::{
    log_error, maker_inc_connections, monitor, oracle, projection, setup_contract, setup_maker,
    wallet, wire, Tasks,
};
use anyhow::{Context as _, Result};
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::schnorrsig;
use futures::channel::mpsc;
use futures::future::RemoteHandle;
use futures::{future, SinkExt};
use maia::secp256k1_zkp::Signature;
use sqlx::pool::PoolConnection;
use sqlx::Sqlite;
use std::collections::{HashMap, HashSet};
use time::Duration;
use xtra::prelude::*;
use xtra::Actor as _;
use xtra_productivity::xtra_productivity;

pub struct AcceptOrder {
    pub order_id: OrderId,
}
pub struct RejectOrder {
    pub order_id: OrderId,
}
pub struct AcceptSettlement {
    pub order_id: OrderId,
}
pub struct RejectSettlement {
    pub order_id: OrderId,
}
pub struct AcceptRollOver {
    pub order_id: OrderId,
}
pub struct RejectRollOver {
    pub order_id: OrderId,
}
pub struct Commit {
    pub order_id: OrderId,
}
pub struct NewOrder {
    pub price: Price,
    pub min_quantity: Usd,
    pub max_quantity: Usd,
    pub fee_rate: u32,
}

pub struct TakerConnected {
    pub id: Identity,
}

pub struct TakerDisconnected {
    pub id: Identity,
}

pub struct CfdRollOverCompleted {
    pub order_id: OrderId,
    pub dlc: Result<Dlc>,
}

pub struct FromTaker {
    pub taker_id: Identity,
    pub msg: wire::TakerToMaker,
}

pub struct Actor<O, M, T, W> {
    db: sqlx::SqlitePool,
    wallet: Address<W>,
    settlement_interval: Duration,
    oracle_pk: schnorrsig::PublicKey,
    projection_actor: Address<projection::Actor>,
    takers: Address<T>,
    current_order_id: Option<OrderId>,
    monitor_actor: Address<M>,
    setup_actors: AddressMap<OrderId, setup_maker::Actor>,
    roll_over_state: RollOverState,
    oracle_actor: Address<O>,
    // Maker needs to also store Identity to be able to send a reply back
    current_pending_proposals: HashMap<OrderId, (UpdateCfdProposal, Identity)>,
    current_agreed_proposals: HashMap<OrderId, (SettlementProposal, Identity)>,
    connected_takers: HashSet<Identity>,
    n_payouts: usize,
    tasks: Tasks,
}

enum RollOverState {
    Active {
        taker: Identity,
        sender: mpsc::UnboundedSender<wire::RollOverMsg>,
        _task: RemoteHandle<()>,
    },
    None,
}

impl<O, M, T, W> Actor<O, M, T, W> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: sqlx::SqlitePool,
        wallet: Address<W>,
        settlement_interval: Duration,
        oracle_pk: schnorrsig::PublicKey,
        projection_actor: Address<projection::Actor>,
        takers: Address<T>,
        monitor_actor: Address<M>,
        oracle_actor: Address<O>,
        n_payouts: usize,
    ) -> Self {
        Self {
            db,
            wallet,
            settlement_interval,
            oracle_pk,
            projection_actor,
            takers,
            current_order_id: None,
            monitor_actor,
            setup_actors: AddressMap::default(),
            roll_over_state: RollOverState::None,
            oracle_actor,
            current_pending_proposals: HashMap::new(),
            current_agreed_proposals: HashMap::new(),
            n_payouts,
            connected_takers: HashSet::new(),
            tasks: Tasks::default(),
        }
    }

    async fn handle_propose_roll_over(
        &mut self,
        proposal: RollOverProposal,
        taker_id: Identity,
    ) -> Result<()> {
        tracing::info!(
            "Received proposal from the taker {}: {:?} to roll over order {}",
            taker_id,
            proposal,
            proposal.order_id
        );

        // check if CFD is in open state, otherwise we should not proceed
        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd_by_order_id(proposal.order_id, &mut conn).await?;
        match cfd {
            Cfd {
                state: CfdState::Open { .. },
                ..
            } => (),
            _ => {
                anyhow::bail!("Order is in invalid state. Cannot propose roll over.")
            }
        };

        let new_proposal = UpdateCfdProposal::RollOverProposal {
            proposal: proposal.clone(),
            direction: SettlementKind::Incoming,
        };

        self.current_pending_proposals
            .insert(proposal.order_id, (new_proposal.clone(), taker_id));
        self.projection_actor
            .send(try_into_update_rollover_proposal(new_proposal)?)
            .await?;

        Ok(())
    }

    async fn handle_propose_settlement(
        &mut self,
        taker_id: Identity,
        proposal: SettlementProposal,
    ) -> Result<()> {
        tracing::info!(
            "Received settlement proposal from the taker: {:?}",
            proposal
        );

        let new_proposal = UpdateCfdProposal::Settlement {
            proposal: proposal.clone(),
            direction: SettlementKind::Incoming,
        };

        self.current_pending_proposals
            .insert(proposal.order_id, (new_proposal.clone(), taker_id));
        self.projection_actor
            .send(try_into_update_settlement_proposal(new_proposal)?)
            .await?;

        Ok(())
    }

    async fn handle_inc_roll_over_protocol_msg(
        &mut self,
        taker_id: Identity,
        msg: wire::RollOverMsg,
    ) -> Result<()> {
        match &mut self.roll_over_state {
            RollOverState::Active { taker, sender, .. } if taker_id == *taker => {
                sender.send(msg).await?;
            }
            RollOverState::Active { taker, .. } => {
                anyhow::bail!("Currently rolling over with different taker {}", taker)
            }
            RollOverState::None => {}
        }

        Ok(())
    }

    /// Removes a proposal and updates the update cfd proposals' feed
    async fn remove_pending_proposal(&mut self, order_id: &OrderId) -> Result<()> {
        let removed_proposal = self.current_pending_proposals.remove(order_id);

        // Strip the identity, ID doesn't care about this implementation detail
        let removed_proposal = removed_proposal.map(|(proposal, _)| proposal);

        if let Some(removed_proposal) = removed_proposal {
            match removed_proposal {
                UpdateCfdProposal::Settlement { .. } => {
                    self.projection_actor
                        .send(UpdateSettlementProposal {
                            order: *order_id,
                            proposal: None,
                        })
                        .await?
                }
                UpdateCfdProposal::RollOverProposal { .. } => {
                    self.projection_actor
                        .send(UpdateRollOverProposal {
                            order: *order_id,
                            proposal: None,
                        })
                        .await?
                }
            }
        } else {
            anyhow::bail!("Could not find proposal with order id: {}", &order_id);
        }
        Ok(())
    }

    fn get_taker_id_of_proposal(&self, order_id: &OrderId) -> Result<Identity> {
        let (_, taker_id) = self
            .current_pending_proposals
            .get(order_id)
            .context("Could not find proposal for given order id")?;
        Ok(*taker_id)
    }

    fn get_settlement_proposal(&self, order_id: OrderId) -> Result<(SettlementProposal, Identity)> {
        let (update_proposal, taker_id) = self
            .current_pending_proposals
            .get(&order_id)
            .context("have a proposal that is about to be accepted")?;

        let proposal = match update_proposal {
            UpdateCfdProposal::Settlement { proposal, .. } => proposal,
            UpdateCfdProposal::RollOverProposal { .. } => {
                anyhow::bail!("did not expect a rollover proposal");
            }
        };
        Ok((proposal.clone(), *taker_id))
    }

    async fn update_connected_takers(&mut self) -> Result<()> {
        self.projection_actor
            .send(Update(
                self.connected_takers
                    .clone()
                    .into_iter()
                    .collect::<Vec<Identity>>(),
            ))
            .await?;
        Ok(())
    }

    async fn append_cfd_state_setup_failed(
        &mut self,
        order_id: OrderId,
        error: anyhow::Error,
    ) -> Result<()> {
        tracing::error!(%order_id, "Contract setup failed: {:#?}", error);

        let mut conn = self.db.acquire().await?;
        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;
        cfd.state = CfdState::setup_failed(error.to_string());
        append_cfd_state(&cfd, &mut conn, &self.projection_actor).await?;

        Ok(())
    }

    async fn append_cfd_state_rejected(&mut self, order_id: OrderId) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;
        cfd.state = CfdState::rejected();
        append_cfd_state(&cfd, &mut conn, &self.projection_actor).await?;

        Ok(())
    }
}

impl<O, M, T, W> Actor<O, M, T, W>
where
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle_monitoring_event(&mut self, event: monitor::Event) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        cfd_actors::handle_monitoring_event(event, &mut conn, &self.wallet, &self.projection_actor)
            .await?;
        Ok(())
    }

    async fn handle_oracle_attestation(&mut self, attestation: oracle::Attestation) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        cfd_actors::handle_oracle_attestation(
            attestation,
            &mut conn,
            &self.wallet,
            &self.projection_actor,
        )
        .await?;
        Ok(())
    }
}

impl<O, M, T, W> Actor<O, M, T, W>
where
    T: xtra::Handler<maker_inc_connections::TakerMessage>,
{
    async fn handle_taker_connected(&mut self, taker_id: Identity) -> Result<()> {
        let mut conn = self.db.acquire().await?;

        let current_order = match self.current_order_id {
            Some(current_order_id) => Some(load_order_by_id(current_order_id, &mut conn).await?),
            None => None,
        };

        // Need to use `do_send_async` here because we are being invoked from the
        // `maker_inc_connections::Actor`. Using `send` would result in a deadlock.
        #[allow(clippy::disallowed_method)]
        self.takers
            .do_send_async(maker_inc_connections::TakerMessage {
                taker_id,
                msg: wire::MakerToTaker::CurrentOrder(current_order),
            })
            .await?;

        if !self.connected_takers.insert(taker_id) {
            tracing::warn!("Taker already connected: {:?}", &taker_id);
        }
        self.update_connected_takers().await?;
        Ok(())
    }

    async fn handle_taker_disconnected(&mut self, taker_id: Identity) -> Result<()> {
        if !self.connected_takers.remove(&taker_id) {
            tracing::warn!("Removed unknown taker: {:?}", &taker_id);
        }
        self.update_connected_takers().await?;
        Ok(())
    }

    async fn reject_order(
        &mut self,
        taker_id: Identity,
        mut cfd: Cfd,
        mut conn: PoolConnection<Sqlite>,
    ) -> Result<()> {
        cfd.state = CfdState::rejected();

        append_cfd_state(&cfd, &mut conn, &self.projection_actor).await?;

        self.takers
            .send(maker_inc_connections::TakerMessage {
                taker_id,
                msg: wire::MakerToTaker::RejectOrder(cfd.order.id),
            })
            .await??;

        Ok(())
    }
}

impl<O, M, T, W> Actor<O, M, T, W>
where
    O: xtra::Handler<oracle::GetAnnouncement> + xtra::Handler<oracle::MonitorAttestation>,
    M: xtra::Handler<monitor::StartMonitoring>,
    T: xtra::Handler<maker_inc_connections::ConfirmOrder>
        + xtra::Handler<maker_inc_connections::TakerMessage>
        + xtra::Handler<maker_inc_connections::BroadcastOrder>
        + xtra::Handler<Stopping<setup_maker::Actor>>,
    W: xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::BuildPartyParams>
        + xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle_take_order(
        &mut self,
        taker_id: Identity,
        order_id: OrderId,
        quantity: Usd,
        ctx: &mut Context<Self>,
    ) -> Result<()> {
        tracing::debug!(%taker_id, %quantity, %order_id, "Taker wants to take an order");

        let disconnected = self
            .setup_actors
            .get_disconnected(order_id)
            .with_context(|| {
                format!(
                    "Contract setup for order {} is already in progress",
                    order_id
                )
            })?;

        let mut conn = self.db.acquire().await?;

        // 1. Validate if order is still valid
        let current_order = match self.current_order_id {
            Some(current_order_id) if current_order_id == order_id => {
                load_order_by_id(current_order_id, &mut conn).await?
            }
            _ => {
                // An outdated order on the taker side does not require any state change on the
                // maker. notifying the taker with a specific message should be sufficient.
                // Since this is a scenario that we should rarely see we log
                // a warning to be sure we don't trigger this code path frequently.
                tracing::warn!("Taker tried to take order with outdated id {}", order_id);

                self.takers
                    .send(maker_inc_connections::TakerMessage {
                        taker_id,
                        msg: wire::MakerToTaker::InvalidOrderId(order_id),
                    })
                    .await??;

                return Ok(());
            }
        };

        // 2. Remove current order
        // The order is removed before we update the state, because the maker might react on the
        // state change. Once we know that we go for either an accept/reject scenario we
        // have to remove the current order.
        self.current_order_id = None;

        // Need to use `do_send_async` here because invoking the
        // corresponding handler can result in a deadlock with another
        // invocation in `maker_inc_connections.rs`
        #[allow(clippy::disallowed_method)]
        self.takers
            .do_send_async(maker_inc_connections::BroadcastOrder(None))
            .await?;

        self.projection_actor.send(projection::Update(None)).await?;

        // 3. Insert CFD in DB
        let cfd = Cfd::new(
            current_order.clone(),
            quantity,
            CfdState::IncomingOrderRequest {
                common: CfdStateCommon {
                    transition_timestamp: Timestamp::now(),
                },
                taker_id,
            },
            taker_id,
        );
        insert_cfd_and_update_feed(&cfd, &mut conn, &self.projection_actor).await?;

        // 4. Try to get the oracle announcement, if that fails we should exit prior to changing any
        // state
        let announcement = self
            .oracle_actor
            .send(oracle::GetAnnouncement(cfd.order.oracle_event_id))
            .await??;

        // 5. Start up contract setup actor
        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");
        let (addr, fut) = setup_maker::Actor::new(
            (cfd.order, cfd.quantity_usd, self.n_payouts),
            (self.oracle_pk, announcement),
            &self.wallet,
            &self.wallet,
            (&self.takers, &self.takers, taker_id),
            &this,
            (&self.takers, &this),
        )
        .create(None)
        .run();

        disconnected.insert(addr);

        self.tasks.add(fut);

        Ok(())
    }
}

#[xtra_productivity]
impl<O, M, T, W> Actor<O, M, T, W> {
    async fn handle_accept_order(&mut self, msg: AcceptOrder) -> Result<()> {
        let AcceptOrder { order_id } = msg;

        tracing::debug!(%order_id, "Maker accepts order");

        let mut conn = self.db.acquire().await?;
        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        self.setup_actors
            .send(&order_id, setup_maker::Accepted)
            .await
            .with_context(|| format!("No active contract setup for order {}", order_id))?;

        cfd.state = CfdState::contract_setup();
        append_cfd_state(&cfd, &mut conn, &self.projection_actor).await?;

        Ok(())
    }
}

#[xtra_productivity(message_impl = false)]
impl<O, M, T, W> Actor<O, M, T, W> {
    async fn handle_setup_actor_stopping(&mut self, message: Stopping<setup_maker::Actor>) {
        self.setup_actors.gc(message);
    }
}

#[xtra_productivity]
impl<O, M, T, W> Actor<O, M, T, W>
where
    T: xtra::Handler<maker_inc_connections::TakerMessage>,
{
    async fn handle_reject_order(&mut self, msg: RejectOrder) -> Result<()> {
        let RejectOrder { order_id } = msg;

        tracing::debug!(%order_id, "Maker rejects order");

        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        let taker_id = match cfd {
            Cfd {
                state: CfdState::IncomingOrderRequest { taker_id, .. },
                ..
            } => taker_id,
            _ => {
                anyhow::bail!("Order is in invalid state. Ignoring trying to reject it.")
            }
        };

        self.reject_order(taker_id, cfd, conn).await?;

        Ok(())
    }

    async fn handle_accept_settlement(&mut self, msg: AcceptSettlement) -> Result<()> {
        let AcceptSettlement { order_id } = msg;

        tracing::debug!(%order_id, "Maker accepts a settlement proposal" );

        let taker_id = self.get_taker_id_of_proposal(&order_id)?;

        match self
            .takers
            .send(maker_inc_connections::TakerMessage {
                taker_id,
                msg: wire::MakerToTaker::Settlement {
                    order_id,
                    msg: wire::maker_to_taker::Settlement::Confirm,
                },
            })
            .await?
        {
            Ok(_) => {
                self.current_agreed_proposals
                    .insert(order_id, self.get_settlement_proposal(order_id)?);
                self.remove_pending_proposal(&order_id)
                    .await
                    .context("accepted settlement")?;
            }
            Err(e) => {
                tracing::warn!("Failed to notify taker of accepted settlement: {}", e);
                self.remove_pending_proposal(&order_id)
                    .await
                    .context("accepted settlement")?;
            }
        }

        Ok(())
    }

    async fn handle_reject_settlement(&mut self, msg: RejectSettlement) -> Result<()> {
        let RejectSettlement { order_id } = msg;

        tracing::debug!(%order_id, "Maker rejects a settlement proposal" );

        let taker_id = self.get_taker_id_of_proposal(&order_id)?;

        // clean-up state ahead of sending to ensure consistency in case we fail to deliver the
        // message
        self.remove_pending_proposal(&order_id)
            .await
            .context("rejected settlement")?;

        self.takers
            .send(maker_inc_connections::TakerMessage {
                taker_id,
                msg: wire::MakerToTaker::Settlement {
                    order_id,
                    msg: wire::maker_to_taker::Settlement::Reject,
                },
            })
            .await??;

        Ok(())
    }

    async fn handle_reject_roll_over(&mut self, msg: RejectRollOver) -> Result<()> {
        let RejectRollOver { order_id } = msg;

        tracing::debug!(%order_id, "Maker rejects a roll_over proposal" );

        // Validate if order is actually being requested to be extended
        let (_, taker_id) = match self.current_pending_proposals.get(&order_id) {
            Some((
                UpdateCfdProposal::RollOverProposal {
                    proposal,
                    direction: SettlementKind::Incoming,
                },
                taker_id,
            )) => (proposal, *taker_id),
            _ => {
                anyhow::bail!("Order is in invalid state. Ignoring reject roll over request.")
            }
        };

        // clean-up state ahead of sending to ensure consistency in case we fail to deliver the
        // message
        self.remove_pending_proposal(&order_id)
            .await
            .context("rejected roll_over")?;

        self.takers
            .send(maker_inc_connections::TakerMessage {
                taker_id,
                msg: wire::MakerToTaker::RejectRollOver(order_id),
            })
            .await??;

        Ok(())
    }
}

#[xtra_productivity]
impl<O, M, T, W> Actor<O, M, T, W>
where
    Self: xtra::Handler<CfdRollOverCompleted>,
    O: xtra::Handler<oracle::MonitorAttestation> + xtra::Handler<oracle::GetAnnouncement>,
    T: xtra::Handler<maker_inc_connections::TakerMessage>,
    W: xtra::Handler<wallet::Sign> + xtra::Handler<wallet::BuildPartyParams>,
{
    async fn handle_accept_roll_over(
        &mut self,
        msg: AcceptRollOver,
        ctx: &mut Context<Self>,
    ) -> Result<()> {
        let AcceptRollOver { order_id } = msg;

        tracing::debug!(%order_id, "Maker accepts a roll_over proposal" );

        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        // Validate if order is actually being requested to be extended
        let (proposal, taker_id) = match self.current_pending_proposals.get(&order_id) {
            Some((
                UpdateCfdProposal::RollOverProposal {
                    proposal,
                    direction: SettlementKind::Incoming,
                },
                taker_id,
            )) => (proposal, *taker_id),
            _ => {
                anyhow::bail!("Order is in invalid state. Ignoring trying to accept the roll over request it.")
            }
        };

        let dlc = cfd.open_dlc().context("CFD was in wrong state")?;

        let oracle_event_id = oracle::next_announcement_after(
            time::OffsetDateTime::now_utc() + cfd.order.settlement_interval,
        )?;
        let announcement = self
            .oracle_actor
            .send(oracle::GetAnnouncement(oracle_event_id))
            .await?
            .with_context(|| format!("Announcement {} not found", oracle_event_id))?;

        self.takers
            .send(maker_inc_connections::TakerMessage {
                taker_id,
                msg: wire::MakerToTaker::ConfirmRollOver {
                    order_id: proposal.order_id,
                    oracle_event_id,
                },
            })
            .await??;

        let (sender, receiver) = mpsc::unbounded();
        let contract_future = setup_contract::roll_over(
            self.takers.clone().into_sink().with(move |msg| {
                future::ok(maker_inc_connections::TakerMessage {
                    taker_id,
                    msg: wire::MakerToTaker::RollOverProtocol { order_id, msg },
                })
            }),
            receiver,
            (self.oracle_pk, announcement),
            RolloverParams::new(
                cfd.order.price,
                cfd.quantity_usd,
                cfd.order.leverage,
                cfd.refund_timelock_in_blocks(),
                cfd.order.fee_rate,
            ),
            Role::Maker,
            dlc,
            self.n_payouts,
        );

        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");

        let task = async move {
            let dlc = contract_future.await;

            this.send(CfdRollOverCompleted { order_id, dlc })
                .await
                .expect("always connected to ourselves")
        }
        .spawn_with_handle();

        self.roll_over_state = RollOverState::Active {
            sender,
            taker: taker_id,
            _task: task,
        };

        self.remove_pending_proposal(&order_id)
            .await
            .context("accepted roll_over")?;
        Ok(())
    }
}

#[xtra_productivity]
impl<O, M, T, W> Actor<O, M, T, W>
where
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle_commit(&mut self, msg: Commit) -> Result<()> {
        let Commit { order_id } = msg;

        let mut conn = self.db.acquire().await?;
        cfd_actors::handle_commit(order_id, &mut conn, &self.wallet, &self.projection_actor)
            .await?;

        Ok(())
    }
}

impl<O, M, T, W> Actor<O, M, T, W>
where
    M: xtra::Handler<monitor::StartMonitoring>,
    O: xtra::Handler<oracle::MonitorAttestation>,
{
    async fn handle_roll_over_completed(
        &mut self,
        order_id: OrderId,
        dlc: Result<Dlc>,
    ) -> Result<()> {
        let dlc = dlc.context("Failed to roll over contract with taker")?;
        self.roll_over_state = RollOverState::None;

        let mut conn = self.db.acquire().await?;
        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;
        cfd.state = CfdState::Open {
            common: CfdStateCommon::default(),
            dlc: dlc.clone(),
            attestation: None,
            collaborative_close: None,
        };
        append_cfd_state(&cfd, &mut conn, &self.projection_actor).await?;

        self.monitor_actor
            .send(monitor::StartMonitoring {
                id: order_id,
                params: MonitorParams::new(dlc.clone(), cfd.refund_timelock_in_blocks()),
            })
            .await?;

        self.oracle_actor
            .send(oracle::MonitorAttestation {
                event_id: dlc.settlement_event_id,
            })
            .await?;

        Ok(())
    }
}

impl<O, M, T, W> Actor<O, M, T, W>
where
    M: xtra::Handler<monitor::CollaborativeSettlement>,
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle_initiate_settlement(
        &mut self,
        taker_id: Identity,
        order_id: OrderId,
        sig_taker: Signature,
    ) -> Result<()> {
        tracing::info!(
            "Taker {} initiated collab settlement for order { } by sending their signature",
            taker_id,
            order_id,
        );

        let (proposal, agreed_taker_id) = self
            .current_agreed_proposals
            .get(&order_id)
            .context("maker should have data matching the agreed settlement")?;

        if taker_id != *agreed_taker_id {
            anyhow::bail!(
                "taker Id mismatch. Expected: {}, received: {}",
                agreed_taker_id,
                taker_id
            );
        }

        let mut conn = self.db.acquire().await?;

        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;
        let dlc = cfd.open_dlc().context("CFD was in wrong state")?;

        let (tx, sig_maker) = dlc.close_transaction(proposal)?;

        let own_script_pubkey = dlc.script_pubkey_for(cfd.role());
        cfd.handle_proposal_signed(CollaborativeSettlement::new(
            tx.clone(),
            own_script_pubkey.clone(),
            proposal.price,
        )?)?;
        append_cfd_state(&cfd, &mut conn, &self.projection_actor).await?;

        let spend_tx = dlc.finalize_spend_transaction((tx, sig_maker), sig_taker)?;

        let txid = self
            .wallet
            .send(wallet::TryBroadcastTransaction {
                tx: spend_tx.clone(),
            })
            .await?
            .context("Broadcasting spend transaction")?;
        tracing::info!("Close transaction published with txid {}", txid);

        self.monitor_actor
            .send(monitor::CollaborativeSettlement {
                order_id,
                tx: (txid, own_script_pubkey),
            })
            .await?;

        self.current_agreed_proposals
            .remove(&order_id)
            .context("remove accepted proposal after signing")?;

        Ok(())
    }
}

#[xtra_productivity]
impl<O, M, T, W> Actor<O, M, T, W>
where
    T: xtra::Handler<maker_inc_connections::BroadcastOrder>,
{
    async fn handle_new_order(&mut self, msg: NewOrder) -> Result<()> {
        let NewOrder {
            price,
            min_quantity,
            max_quantity,
            fee_rate,
        } = msg;

        let oracle_event_id = oracle::next_announcement_after(
            time::OffsetDateTime::now_utc() + self.settlement_interval,
        )?;

        let order = Order::new(
            price,
            min_quantity,
            max_quantity,
            Origin::Ours,
            oracle_event_id,
            self.settlement_interval,
            fee_rate,
        )?;

        // 1. Save to DB
        let mut conn = self.db.acquire().await?;
        insert_order(&order, &mut conn).await?;

        // 2. Update actor state to current order
        self.current_order_id.replace(order.id);

        // 3. Notify UI via feed
        self.projection_actor
            .send(projection::Update(Some(order.clone())))
            .await?;

        // 4. Inform connected takers
        self.takers
            .send(maker_inc_connections::BroadcastOrder(Some(order)))
            .await?;

        Ok(())
    }
}

#[xtra_productivity]
impl<O, M, T, W> Actor<O, M, T, W>
where
    O: xtra::Handler<oracle::MonitorAttestation>,
    M: xtra::Handler<monitor::StartMonitoring>,
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle_setup_completed(&mut self, msg: setup_maker::Completed) {
        log_error!(async {
            use setup_maker::Completed::*;
            let (order_id, dlc) = match msg {
                NewContract { order_id, dlc } => (order_id, dlc),
                Failed { order_id, error } => {
                    self.append_cfd_state_setup_failed(order_id, error).await?;
                    return anyhow::Ok(());
                }
                Rejected(order_id) => {
                    self.append_cfd_state_rejected(order_id).await?;
                    return anyhow::Ok(());
                }
            };

            let mut conn = self.db.acquire().await?;
            let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

            cfd.state = CfdState::PendingOpen {
                common: CfdStateCommon::default(),
                dlc: dlc.clone(),
                attestation: None,
            };

            append_cfd_state(&cfd, &mut conn, &self.projection_actor).await?;

            let txid = self
                .wallet
                .send(wallet::TryBroadcastTransaction {
                    tx: dlc.lock.0.clone(),
                })
                .await??;

            tracing::info!("Lock transaction published with txid {}", txid);

            self.monitor_actor
                .send(monitor::StartMonitoring {
                    id: order_id,
                    params: MonitorParams::new(dlc.clone(), cfd.refund_timelock_in_blocks()),
                })
                .await?;

            self.oracle_actor
                .send(oracle::MonitorAttestation {
                    event_id: dlc.settlement_event_id,
                })
                .await?;

            Ok(())
        });
    }
}

#[async_trait]
impl<O: 'static, M: 'static, T: 'static, W: 'static> Handler<TakerConnected> for Actor<O, M, T, W>
where
    T: xtra::Handler<maker_inc_connections::TakerMessage>,
{
    async fn handle(&mut self, msg: TakerConnected, _ctx: &mut Context<Self>) {
        log_error!(self.handle_taker_connected(msg.id));
    }
}

#[async_trait]
impl<O: 'static, M: 'static, T: 'static, W: 'static> Handler<TakerDisconnected>
    for Actor<O, M, T, W>
where
    T: xtra::Handler<maker_inc_connections::TakerMessage>,
{
    async fn handle(&mut self, msg: TakerDisconnected, _ctx: &mut Context<Self>) {
        log_error!(self.handle_taker_disconnected(msg.id));
    }
}

#[async_trait]
impl<O: 'static, M: 'static, T: 'static, W: 'static> Handler<CfdRollOverCompleted>
    for Actor<O, M, T, W>
where
    M: xtra::Handler<monitor::StartMonitoring>,
    O: xtra::Handler<oracle::MonitorAttestation>,
{
    async fn handle(&mut self, msg: CfdRollOverCompleted, _ctx: &mut Context<Self>) {
        log_error!(self.handle_roll_over_completed(msg.order_id, msg.dlc));
    }
}

#[async_trait]
impl<O: 'static, M: 'static, T: 'static, W: 'static> Handler<monitor::Event> for Actor<O, M, T, W>
where
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle(&mut self, msg: monitor::Event, _ctx: &mut Context<Self>) {
        log_error!(self.handle_monitoring_event(msg))
    }
}

#[async_trait]
impl<O: 'static, M: 'static, T: 'static, W: 'static> Handler<FromTaker> for Actor<O, M, T, W>
where
    O: xtra::Handler<oracle::GetAnnouncement> + xtra::Handler<oracle::MonitorAttestation>,
    M: xtra::Handler<monitor::StartMonitoring> + xtra::Handler<monitor::CollaborativeSettlement>,
    T: xtra::Handler<maker_inc_connections::ConfirmOrder>
        + xtra::Handler<maker_inc_connections::TakerMessage>
        + xtra::Handler<maker_inc_connections::BroadcastOrder>
        + xtra::Handler<Stopping<setup_maker::Actor>>,
    W: xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::BuildPartyParams>
        + xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle(&mut self, FromTaker { taker_id, msg }: FromTaker, ctx: &mut Context<Self>) {
        match msg {
            wire::TakerToMaker::TakeOrder { order_id, quantity } => {
                log_error!(self.handle_take_order(taker_id, order_id, quantity, ctx))
            }
            wire::TakerToMaker::Settlement {
                order_id,
                msg:
                    wire::taker_to_maker::Settlement::Propose {
                        timestamp,
                        taker,
                        maker,
                        price,
                    },
            } => {
                log_error!(self.handle_propose_settlement(
                    taker_id,
                    SettlementProposal {
                        order_id,
                        timestamp,
                        taker,
                        maker,
                        price
                    }
                ))
            }
            wire::TakerToMaker::Settlement {
                order_id,
                msg: wire::taker_to_maker::Settlement::Initiate { sig_taker },
            } => {
                log_error!(self.handle_initiate_settlement(taker_id, order_id, sig_taker))
            }
            wire::TakerToMaker::ProposeRollOver {
                order_id,
                timestamp,
            } => {
                log_error!(self.handle_propose_roll_over(
                    RollOverProposal {
                        order_id,
                        timestamp,
                    },
                    taker_id,
                ))
            }
            wire::TakerToMaker::RollOverProtocol(msg) => {
                log_error!(self.handle_inc_roll_over_protocol_msg(taker_id, msg))
            }
            wire::TakerToMaker::Protocol { .. } => {
                unreachable!("This kind of message should be sent to the `setup_maker::Actor`")
            }
            TakerToMaker::Hello(_) => {
                unreachable!("The Hello message is not sent to the cfd actor")
            }
        }
    }
}

#[async_trait]
impl<O: 'static, M: 'static, T: 'static, W: 'static> Handler<oracle::Attestation>
    for Actor<O, M, T, W>
where
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle(&mut self, msg: oracle::Attestation, _ctx: &mut Context<Self>) {
        log_error!(self.handle_oracle_attestation(msg))
    }
}

impl Message for TakerConnected {
    type Result = ();
}

impl Message for TakerDisconnected {
    type Result = ();
}

impl Message for CfdRollOverCompleted {
    type Result = ();
}

impl Message for FromTaker {
    type Result = ();
}

impl<O: 'static, M: 'static, T: 'static, W: 'static> xtra::Actor for Actor<O, M, T, W> {}
