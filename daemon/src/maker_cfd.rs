use crate::cfd_actors::{self, append_cfd_state, insert_cfd_and_send_to_feed};
use crate::db::{insert_order, load_cfd_by_order_id, load_order_by_id};
use crate::model::cfd::{
    Cfd, CfdState, CfdStateCommon, CollaborativeSettlement, Dlc, Order, OrderId, Origin, Role,
    RollOverProposal, SettlementKind, SettlementProposal, UpdateCfdProposal,
};
use crate::model::{Price, TakerId, Timestamp, Usd};
use crate::monitor::MonitorParams;
use crate::setup_contract::{RolloverParams, SetupParams};
use crate::tokio_ext::FutureExt;
use crate::{
    log_error, maker_inc_connections, monitor, oracle, projection, setup_contract, wallet, wire,
    UpdateCfdProposals,
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
use tokio::sync::watch;
use xtra::prelude::*;

pub enum CfdAction {
    AcceptOrder { order_id: OrderId },
    RejectOrder { order_id: OrderId },
    AcceptSettlement { order_id: OrderId },
    RejectSettlement { order_id: OrderId },
    AcceptRollOver { order_id: OrderId },
    RejectRollOver { order_id: OrderId },
    Commit { order_id: OrderId },
}

pub struct NewOrder {
    pub price: Price,
    pub min_quantity: Usd,
    pub max_quantity: Usd,
}

pub struct TakerConnected {
    pub id: TakerId,
}

pub struct TakerDisconnected {
    pub id: TakerId,
}

pub struct CfdSetupCompleted {
    pub order_id: OrderId,
    pub dlc: Result<Dlc>,
}

pub struct CfdRollOverCompleted {
    pub order_id: OrderId,
    pub dlc: Result<Dlc>,
}

pub struct FromTaker {
    pub taker_id: TakerId,
    pub msg: wire::TakerToMaker,
}

pub struct Actor<O, M, T, W> {
    db: sqlx::SqlitePool,
    wallet: Address<W>,
    settlement_interval: Duration,
    oracle_pk: schnorrsig::PublicKey,
    projection_actor: Address<projection::Actor>,
    connected_takers_feed_sender: watch::Sender<Vec<TakerId>>,
    takers: Address<T>,
    current_order_id: Option<OrderId>,
    monitor_actor: Address<M>,
    setup_state: SetupState,
    roll_over_state: RollOverState,
    oracle_actor: Address<O>,
    // Maker needs to also store TakerId to be able to send a reply back
    current_pending_proposals: HashMap<OrderId, (UpdateCfdProposal, TakerId)>,
    current_agreed_proposals: HashMap<OrderId, (SettlementProposal, TakerId)>,
    // TODO: Keep in projection actor
    connected_takers: HashSet<TakerId>,
    n_payouts: usize,
}

enum SetupState {
    Active {
        taker: TakerId,
        sender: mpsc::UnboundedSender<wire::SetupMsg>,
        _task: RemoteHandle<()>,
    },
    None,
}

enum RollOverState {
    Active {
        taker: TakerId,
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
        connected_takers_feed_sender: watch::Sender<Vec<TakerId>>,
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
            connected_takers_feed_sender,
            takers,
            current_order_id: None,
            monitor_actor,
            setup_state: SetupState::None,
            roll_over_state: RollOverState::None,
            oracle_actor,
            current_pending_proposals: HashMap::new(),
            current_agreed_proposals: HashMap::new(),
            n_payouts,
            connected_takers: HashSet::new(),
        }
    }

    async fn handle_propose_roll_over(
        &mut self,
        proposal: RollOverProposal,
        taker_id: TakerId,
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

        self.current_pending_proposals.insert(
            proposal.order_id,
            (
                UpdateCfdProposal::RollOverProposal {
                    proposal,
                    direction: SettlementKind::Incoming,
                },
                taker_id,
            ),
        );
        self.send_pending_proposals().await?;

        Ok(())
    }

    async fn handle_propose_settlement(
        &mut self,
        taker_id: TakerId,
        proposal: SettlementProposal,
    ) -> Result<()> {
        tracing::info!(
            "Received settlement proposal from the taker: {:?}",
            proposal
        );
        self.current_pending_proposals.insert(
            proposal.order_id,
            (
                UpdateCfdProposal::Settlement {
                    proposal,
                    direction: SettlementKind::Incoming,
                },
                taker_id,
            ),
        );
        self.send_pending_proposals().await?;

        Ok(())
    }

    async fn handle_inc_protocol_msg(
        &mut self,
        taker_id: TakerId,
        msg: wire::SetupMsg,
    ) -> Result<()> {
        match &mut self.setup_state {
            SetupState::Active { taker, sender, .. } if taker_id == *taker => {
                sender.send(msg).await?;
            }
            SetupState::Active { taker, .. } => {
                anyhow::bail!("Currently setting up contract with taker {}", taker)
            }
            SetupState::None => {
                anyhow::bail!("Received setup message without an active contract setup");
            }
        }

        Ok(())
    }

    async fn handle_inc_roll_over_protocol_msg(
        &mut self,
        taker_id: TakerId,
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

    /// Send pending proposals for the purposes of UI updates.
    /// Filters out the TakerIds, as they are an implementation detail inside of
    /// the actor
    async fn send_pending_proposals(&self) -> Result<()> {
        let pending_proposal = self
            .current_pending_proposals
            .iter()
            .map(|(order_id, (update_cfd, _))| (*order_id, (update_cfd.clone())))
            .collect::<UpdateCfdProposals>();
        let _ = self
            .projection_actor
            .send(projection::Update(pending_proposal))
            .await?;
        Ok(())
    }

    /// Removes a proposal and updates the update cfd proposals' feed
    async fn remove_pending_proposal(&mut self, order_id: &OrderId) -> Result<()> {
        if self.current_pending_proposals.remove(order_id).is_none() {
            anyhow::bail!("Could not find proposal with order id: {}", &order_id)
        }
        self.send_pending_proposals().await?;
        Ok(())
    }

    fn get_taker_id_of_proposal(&self, order_id: &OrderId) -> Result<TakerId> {
        let (_, taker_id) = self
            .current_pending_proposals
            .get(order_id)
            .context("Could not find proposal for given order id")?;
        Ok(*taker_id)
    }

    fn get_settlement_proposal(&self, order_id: OrderId) -> Result<(SettlementProposal, TakerId)> {
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
}

impl<O, M, T, W> Actor<O, M, T, W>
where
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle_commit(&mut self, order_id: OrderId) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        cfd_actors::handle_commit(order_id, &mut conn, &self.wallet, &self.projection_actor)
            .await?;

        Ok(())
    }

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
    async fn handle_taker_connected(&mut self, taker_id: TakerId) -> Result<()> {
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
        self.connected_takers_feed_sender
            .send(self.connected_takers.clone().into_iter().collect())?;

        Ok(())
    }

    async fn handle_taker_disconnected(&mut self, taker_id: TakerId) -> Result<()> {
        if !self.connected_takers.remove(&taker_id) {
            tracing::warn!("Removed unknown taker: {:?}", &taker_id);
        }
        self.connected_takers_feed_sender
            .send(self.connected_takers.clone().into_iter().collect())?;
        Ok(())
    }

    async fn handle_accept_settlement(&mut self, order_id: OrderId) -> Result<()> {
        tracing::debug!(%order_id, "Maker accepts a settlement proposal" );

        let taker_id = self.get_taker_id_of_proposal(&order_id)?;

        match self
            .takers
            .send(maker_inc_connections::TakerMessage {
                taker_id,
                msg: wire::MakerToTaker::ConfirmSettlement(order_id),
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

    async fn handle_reject_settlement(&mut self, order_id: OrderId) -> Result<()> {
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
                msg: wire::MakerToTaker::RejectSettlement(order_id),
            })
            .await??;

        Ok(())
    }

    async fn handle_reject_roll_over(&mut self, order_id: OrderId) -> Result<()> {
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

impl<O, M, T, W> Actor<O, M, T, W>
where
    T: xtra::Handler<maker_inc_connections::TakerMessage>
        + xtra::Handler<maker_inc_connections::BroadcastOrder>,
{
    async fn handle_take_order(
        &mut self,
        taker_id: TakerId,
        order_id: OrderId,
        quantity: Usd,
    ) -> Result<()> {
        tracing::debug!(%taker_id, %quantity, %order_id, "Taker wants to take an order");

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
        self.takers
            .send(maker_inc_connections::BroadcastOrder(None))
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
        );
        insert_cfd_and_send_to_feed(&cfd, &mut conn, &self.projection_actor).await?;

        // 4. check if order has acceptable amounts and if not reject the cfd
        // Since rejection is tied to the cfd state at the moment we can only do this after creating
        // a cfd.
        if quantity < current_order.min_quantity || quantity > current_order.max_quantity {
            tracing::warn!(
                "Order rejected because quantity {} was out of bounds. It was either <{} or >{}",
                quantity,
                current_order.min_quantity,
                current_order.max_quantity
            );

            self.reject_order(taker_id, cfd, conn).await?;
        }

        Ok(())
    }

    async fn handle_reject_order(&mut self, order_id: OrderId) -> Result<()> {
        tracing::debug!(%order_id, "Maker rejects an order" );

        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        let taker_id = match cfd {
            Cfd {
                state: CfdState::IncomingOrderRequest { taker_id, .. },
                ..
            } => taker_id,
            _ => {
                anyhow::bail!("Order is in invalid state. Ignoring trying to accept it.")
            }
        };

        self.reject_order(taker_id, cfd, conn).await?;

        Ok(())
    }

    async fn reject_order(
        &mut self,
        taker_id: TakerId,
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
    Self: xtra::Handler<CfdSetupCompleted>,
    O: xtra::Handler<oracle::GetAnnouncement>,
    T: xtra::Handler<maker_inc_connections::TakerMessage>,
    W: xtra::Handler<wallet::Sign> + xtra::Handler<wallet::BuildPartyParams>,
{
    async fn handle_accept_order(
        &mut self,
        order_id: OrderId,
        ctx: &mut Context<Self>,
    ) -> Result<()> {
        if let SetupState::Active { .. } = self.setup_state {
            anyhow::bail!("Already setting up a contract!")
        }

        tracing::debug!(%order_id, "Maker accepts an order" );

        let mut conn = self.db.acquire().await?;

        // 1. Validate if order is still valid
        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;
        let taker_id = match cfd {
            Cfd {
                state: CfdState::IncomingOrderRequest { taker_id, .. },
                ..
            } => taker_id,
            _ => {
                anyhow::bail!("Order is in invalid state. Ignoring trying to accept it.")
            }
        };

        // 2. Try to get the oracle announcement, if that fails we should exit prior to changing any
        // state
        let offer_announcement = self
            .oracle_actor
            .send(oracle::GetAnnouncement(cfd.order.oracle_event_id))
            .await??;

        // 3. Notify the taker that we are ready for contract setup
        // Use `.send` here to ensure we only continue once the message has been sent
        // Nothing done after this call should be able to fail, otherwise we notified the taker, but
        // might not transition to `Active` ourselves!
        self.takers
            .send(maker_inc_connections::TakerMessage {
                taker_id,
                msg: wire::MakerToTaker::ConfirmOrder(cfd.order.id),
            })
            .await??;

        // 4. Insert that we are in contract setup and refresh our own feed
        cfd.state = CfdState::contract_setup();
        append_cfd_state(&cfd, &mut conn, &self.projection_actor).await?;

        // 5. Spawn away the contract setup
        let (sender, receiver) = mpsc::unbounded();
        let contract_future = setup_contract::new(
            self.takers.clone().into_sink().with(move |msg| {
                future::ok(maker_inc_connections::TakerMessage {
                    taker_id,
                    msg: wire::MakerToTaker::Protocol(msg),
                })
            }),
            receiver,
            (self.oracle_pk, offer_announcement),
            SetupParams::new(
                cfd.margin()?,
                cfd.counterparty_margin()?,
                cfd.order.price,
                cfd.quantity_usd,
                cfd.order.leverage,
                cfd.refund_timelock_in_blocks(),
            ),
            self.wallet.clone(),
            Role::Maker,
            self.n_payouts,
        );

        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");

        let task = async move {
            let dlc = contract_future.await;

            this.send(CfdSetupCompleted { order_id, dlc })
                .await
                .expect("always connected to ourselves");
        }
        .spawn_with_handle();

        // 6. Record that we are in an active contract setup
        self.setup_state = SetupState::Active {
            sender,
            taker: taker_id,
            _task: task,
        };

        Ok(())
    }
}

impl<O, M, T, W> Actor<O, M, T, W>
where
    O: xtra::Handler<oracle::MonitorAttestation>,
    M: xtra::Handler<monitor::StartMonitoring>,
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle_cfd_setup_completed(
        &mut self,
        order_id: OrderId,
        dlc: Result<Dlc>,
    ) -> Result<()> {
        self.setup_state = SetupState::None;

        let mut conn = self.db.acquire().await?;
        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        let dlc = match dlc {
            Ok(dlc) => dlc,
            Err(e) => {
                cfd.state = CfdState::SetupFailed {
                    common: CfdStateCommon::default(),
                    info: e.to_string(),
                };

                append_cfd_state(&cfd, &mut conn, &self.projection_actor).await?;

                return Err(e);
            }
        };

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
                params: MonitorParams::new(
                    dlc,
                    cfd.refund_timelock_in_blocks(),
                    cfd.order.oracle_event_id,
                ),
            })
            .await?;

        self.oracle_actor
            .send(oracle::MonitorAttestation {
                event_id: cfd.order.oracle_event_id,
            })
            .await?;

        Ok(())
    }
}

impl<O, M, T, W> Actor<O, M, T, W>
where
    Self: xtra::Handler<CfdRollOverCompleted>,
    O: xtra::Handler<oracle::MonitorAttestation> + xtra::Handler<oracle::GetAnnouncement>,
    T: xtra::Handler<maker_inc_connections::TakerMessage>,
{
    async fn handle_accept_roll_over(
        &mut self,
        order_id: OrderId,
        ctx: &mut Context<Self>,
    ) -> Result<()> {
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

        self.oracle_actor
            .send(oracle::MonitorAttestation {
                event_id: announcement.id,
            })
            .await?;

        let (sender, receiver) = mpsc::unbounded();
        let contract_future = setup_contract::roll_over(
            self.takers.clone().into_sink().with(move |msg| {
                future::ok(maker_inc_connections::TakerMessage {
                    taker_id,
                    msg: wire::MakerToTaker::RollOverProtocol(msg),
                })
            }),
            receiver,
            (self.oracle_pk, announcement),
            RolloverParams::new(
                cfd.order.price,
                cfd.quantity_usd,
                cfd.order.leverage,
                cfd.refund_timelock_in_blocks(),
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

impl<O, M, T, W> Actor<O, M, T, W>
where
    M: xtra::Handler<monitor::StartMonitoring>,
{
    async fn handle_cfd_roll_over_completed(
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
                params: MonitorParams::new(
                    dlc,
                    cfd.refund_timelock_in_blocks(),
                    cfd.order.oracle_event_id,
                ),
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
        taker_id: TakerId,
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

#[async_trait]
impl<O: 'static, M: 'static, T: 'static, W: 'static> Handler<CfdAction> for Actor<O, M, T, W>
where
    Self: xtra::Handler<CfdSetupCompleted> + xtra::Handler<CfdRollOverCompleted>,
    O: xtra::Handler<oracle::MonitorAttestation> + xtra::Handler<oracle::GetAnnouncement>,
    T: xtra::Handler<maker_inc_connections::TakerMessage>
        + xtra::Handler<maker_inc_connections::BroadcastOrder>,
    W: xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::BuildPartyParams>
        + xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle(&mut self, msg: CfdAction, ctx: &mut Context<Self>) -> Result<()> {
        use CfdAction::*;
        if let Err(e) = match msg {
            AcceptOrder { order_id } => self.handle_accept_order(order_id, ctx).await,
            RejectOrder { order_id } => self.handle_reject_order(order_id).await,
            AcceptSettlement { order_id } => self.handle_accept_settlement(order_id).await,
            RejectSettlement { order_id } => self.handle_reject_settlement(order_id).await,
            AcceptRollOver { order_id } => self.handle_accept_roll_over(order_id, ctx).await,
            RejectRollOver { order_id } => self.handle_reject_roll_over(order_id).await,
            Commit { order_id } => self.handle_commit(order_id).await,
        } {
            anyhow::bail!("Message handler failed: {:#}", e);
        }
        Ok(())
    }
}

#[async_trait]
impl<O: 'static, M: 'static, T: 'static, W: 'static> Handler<NewOrder> for Actor<O, M, T, W>
where
    T: xtra::Handler<maker_inc_connections::BroadcastOrder>,
{
    async fn handle(&mut self, msg: NewOrder, _ctx: &mut Context<Self>) -> Result<()> {
        let NewOrder {
            price,
            min_quantity,
            max_quantity,
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
impl<O: 'static, M: 'static, T: 'static, W: 'static> Handler<CfdSetupCompleted>
    for Actor<O, M, T, W>
where
    O: xtra::Handler<oracle::MonitorAttestation>,
    M: xtra::Handler<monitor::StartMonitoring>,
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle(&mut self, msg: CfdSetupCompleted, _ctx: &mut Context<Self>) {
        log_error!(self.handle_cfd_setup_completed(msg.order_id, msg.dlc));
    }
}

#[async_trait]
impl<O: 'static, M: 'static, T: 'static, W: 'static> Handler<CfdRollOverCompleted>
    for Actor<O, M, T, W>
where
    M: xtra::Handler<monitor::StartMonitoring>,
{
    async fn handle(&mut self, msg: CfdRollOverCompleted, _ctx: &mut Context<Self>) {
        log_error!(self.handle_cfd_roll_over_completed(msg.order_id, msg.dlc));
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
    T: xtra::Handler<maker_inc_connections::BroadcastOrder>
        + xtra::Handler<maker_inc_connections::TakerMessage>,
    M: xtra::Handler<monitor::CollaborativeSettlement>,
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle(&mut self, FromTaker { taker_id, msg }: FromTaker, _ctx: &mut Context<Self>) {
        match msg {
            wire::TakerToMaker::TakeOrder { order_id, quantity } => {
                log_error!(self.handle_take_order(taker_id, order_id, quantity))
            }
            wire::TakerToMaker::ProposeSettlement {
                order_id,
                timestamp,
                taker,
                maker,
                price,
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
            wire::TakerToMaker::InitiateSettlement {
                order_id,
                sig_taker,
            } => {
                log_error!(self.handle_initiate_settlement(taker_id, order_id, sig_taker))
            }
            wire::TakerToMaker::Protocol(msg) => {
                log_error!(self.handle_inc_protocol_msg(taker_id, msg))
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

impl Message for NewOrder {
    type Result = Result<()>;
}

impl Message for TakerConnected {
    type Result = ();
}

impl Message for TakerDisconnected {
    type Result = ();
}

impl Message for CfdSetupCompleted {
    type Result = ();
}

impl Message for CfdRollOverCompleted {
    type Result = ();
}

impl Message for CfdAction {
    type Result = Result<()>;
}

impl Message for FromTaker {
    type Result = ();
}

impl<O: 'static, M: 'static, T: 'static, W: 'static> xtra::Actor for Actor<O, M, T, W> {}
