use crate::cfd_actors::{self, append_cfd_state, insert_cfd_and_send_to_feed};
use crate::db::{insert_order, load_cfd_by_order_id, load_order_by_id};
use crate::model::cfd::{
    Cfd, CfdState, CfdStateCommon, CollaborativeSettlement, Completed, Dlc, Order, OrderId, Origin,
    Role, RollOverProposal, SettlementKind, SettlementProposal, UpdateCfdProposal,
    UpdateCfdProposals,
};
use crate::model::{BitMexPriceEventId, Price, Timestamp, Usd};
use crate::monitor::{self, MonitorParams};
use crate::setup_contract::RolloverParams;
use crate::tokio_ext::FutureExt;
use crate::wire::RollOverMsg;
use crate::{
    connection, log_error, oracle, projection, setup_contract, setup_taker, wallet, wire, Tasks,
};
use anyhow::{bail, Context as _, Result};
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::schnorrsig;
use futures::channel::mpsc;
use futures::future::RemoteHandle;
use futures::{future, SinkExt};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use xtra::prelude::*;
use xtra::Actor as _;

pub struct TakeOffer {
    pub order_id: OrderId,
    pub quantity: Usd,
}

pub enum CfdAction {
    ProposeSettlement {
        order_id: OrderId,
        current_price: Price,
    },
    ProposeRollOver {
        order_id: OrderId,
    },
    Commit {
        order_id: OrderId,
    },
}

pub struct CfdRollOverCompleted {
    pub order_id: OrderId,
    pub dlc: Result<Dlc>,
}

enum RollOverState {
    Active {
        sender: mpsc::UnboundedSender<RollOverMsg>,
        _task: RemoteHandle<()>,
    },
    None,
}

pub struct Actor<O, M, W> {
    db: sqlx::SqlitePool,
    wallet: Address<W>,
    oracle_pk: schnorrsig::PublicKey,
    projection_actor: Address<projection::Actor>,
    conn_actor: Address<connection::Actor>,
    monitor_actor: Address<M>,
    setup_actors: HashMap<OrderId, xtra::Address<setup_taker::Actor>>,
    roll_over_state: RollOverState,
    oracle_actor: Address<O>,
    current_pending_proposals: UpdateCfdProposals,
    n_payouts: usize,
    tasks: Tasks,
}

impl<O, M, W> Actor<O, M, W>
where
    W: xtra::Handler<wallet::TryBroadcastTransaction>
        + xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::BuildPartyParams>,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: sqlx::SqlitePool,
        wallet: Address<W>,
        oracle_pk: schnorrsig::PublicKey,
        projection_actor: Address<projection::Actor>,
        conn_actor: Address<connection::Actor>,
        monitor_actor: Address<M>,
        oracle_actor: Address<O>,
        n_payouts: usize,
    ) -> Self {
        Self {
            db,
            wallet,
            oracle_pk,
            projection_actor,
            conn_actor,
            monitor_actor,
            roll_over_state: RollOverState::None,
            oracle_actor,
            current_pending_proposals: HashMap::new(),
            n_payouts,
            setup_actors: HashMap::new(),
            tasks: Tasks::default(),
        }
    }
}

impl<O, M, W> Actor<O, M, W> {
    async fn send_pending_update_proposals(&self) -> Result<()> {
        Ok(self
            .projection_actor
            .send(projection::Update(self.current_pending_proposals.clone()))
            .await?)
    }

    /// Removes a proposal and updates the update cfd proposals' feed
    async fn remove_pending_proposal(&mut self, order_id: &OrderId) -> Result<()> {
        if self.current_pending_proposals.remove(order_id).is_none() {
            anyhow::bail!("Could not find proposal with order id: {}", &order_id)
        }
        self.send_pending_update_proposals().await?;
        Ok(())
    }

    fn get_settlement_proposal(&self, order_id: OrderId) -> Result<&SettlementProposal> {
        match self
            .current_pending_proposals
            .get(&order_id)
            .context("have a proposal that is about to be accepted")?
        {
            UpdateCfdProposal::Settlement { proposal, .. } => Ok(proposal),
            UpdateCfdProposal::RollOverProposal { .. } => {
                anyhow::bail!("did not expect a rollover proposal");
            }
        }
    }
}

impl<O, M, W> Actor<O, M, W>
where
    W: xtra::Handler<wallet::TryBroadcastTransaction>
        + xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::BuildPartyParams>,
{
    async fn handle_commit(&mut self, order_id: OrderId) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        cfd_actors::handle_commit(order_id, &mut conn, &self.wallet, &self.projection_actor)
            .await?;
        Ok(())
    }

    async fn handle_propose_settlement(
        &mut self,
        order_id: OrderId,
        current_price: Price,
    ) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        if !cfd.is_collaborative_settle_possible() {
            anyhow::bail!(
                "Settlement proposal not possible because for cfd {} is in state {} which cannot be collaboratively settled",
                order_id,
                cfd.state
            )
        }

        let proposal = cfd.calculate_settlement(current_price, self.n_payouts)?;

        if self
            .current_pending_proposals
            .contains_key(&proposal.order_id)
        {
            anyhow::bail!(
                "Settlement proposal for order id {} already present",
                order_id
            )
        }

        self.current_pending_proposals.insert(
            proposal.order_id,
            UpdateCfdProposal::Settlement {
                proposal: proposal.clone(),
                direction: SettlementKind::Outgoing,
            },
        );
        self.send_pending_update_proposals().await?;

        self.conn_actor
            .send(wire::TakerToMaker::ProposeSettlement {
                order_id: proposal.order_id,
                timestamp: proposal.timestamp,
                taker: proposal.taker,
                maker: proposal.maker,
                price: proposal.price,
            })
            .await?;
        Ok(())
    }

    async fn handle_settlement_rejected(&mut self, order_id: OrderId) -> Result<()> {
        tracing::info!(%order_id, "Settlement proposal got rejected");

        self.remove_pending_proposal(&order_id).await?;

        Ok(())
    }

    async fn handle_roll_over_rejected(&mut self, order_id: OrderId) -> Result<()> {
        tracing::info!(%order_id, "Roll over proposal got rejected");

        self.remove_pending_proposal(&order_id)
            .await
            .context("rejected settlement")?;

        Ok(())
    }

    async fn handle_inc_roll_over_msg(&mut self, msg: RollOverMsg) -> Result<()> {
        match &mut self.roll_over_state {
            RollOverState::Active { sender, .. } => {
                sender.send(msg).await?;
            }
            RollOverState::None => {
                anyhow::bail!("Received message without an active roll_over setup")
            }
        }

        Ok(())
    }

    async fn handle_invalid_order_id(&mut self, order_id: OrderId) -> Result<()> {
        tracing::debug!(%order_id, "Invalid order ID");

        self.append_cfd_state_rejected(order_id).await?;

        Ok(())
    }
}

impl<O, M, W> Actor<O, M, W> {
    async fn handle_new_order(&mut self, order: Option<Order>) -> Result<()> {
        tracing::trace!("new order {:?}", order);
        match order {
            Some(mut order) => {
                order.origin = Origin::Theirs;

                let mut conn = self.db.acquire().await?;

                if load_cfd_by_order_id(order.id, &mut conn).await.is_ok() {
                    bail!("Received order {} from maker, but already have a cfd in the database for that order. The maker did not properly remove the order.", order.id)
                }

                if load_order_by_id(order.id, &mut conn).await.is_err() {
                    // only insert the order if we don't know it yet
                    insert_order(&order, &mut conn).await?;
                }

                self.projection_actor
                    .send(projection::Update(Some(order)))
                    .await?;
            }
            None => {
                self.projection_actor.send(projection::Update(None)).await?;
            }
        }
        Ok(())
    }

    async fn append_cfd_state_rejected(&mut self, order_id: OrderId) -> Result<()> {
        tracing::debug!(%order_id, "Order rejected");

        let mut conn = self.db.acquire().await?;
        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;
        cfd.state = CfdState::rejected();
        append_cfd_state(&cfd, &mut conn, &self.projection_actor).await?;

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

    /// Set the state of the CFD in the database to `ContractSetup`
    /// and update the corresponding projection.
    async fn handle_setup_started(&mut self, order_id: OrderId) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;
        cfd.state = CfdState::contract_setup();
        append_cfd_state(&cfd, &mut conn, &self.projection_actor).await?;

        Ok(())
    }
}

impl<O, M, W> Actor<O, M, W>
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

    async fn handle_propose_roll_over(&mut self, order_id: OrderId) -> Result<()> {
        if self.current_pending_proposals.contains_key(&order_id) {
            anyhow::bail!("An update for order id {} is already in progress", order_id)
        }

        let proposal = RollOverProposal {
            order_id,
            timestamp: Timestamp::now(),
        };

        self.current_pending_proposals.insert(
            proposal.order_id,
            UpdateCfdProposal::RollOverProposal {
                proposal: proposal.clone(),
                direction: SettlementKind::Outgoing,
            },
        );
        self.send_pending_update_proposals().await?;

        self.conn_actor
            .send(wire::TakerToMaker::ProposeRollOver {
                order_id: proposal.order_id,
                timestamp: proposal.timestamp,
            })
            .await?;
        Ok(())
    }
}

impl<O, M, W> Actor<O, M, W>
where
    Self: xtra::Handler<Completed>,
    O: xtra::Handler<oracle::GetAnnouncement> + xtra::Handler<oracle::MonitorAttestation>,
    W: xtra::Handler<wallet::BuildPartyParams> + xtra::Handler<wallet::Sign>,
{
    async fn handle_take_offer(
        &mut self,
        order_id: OrderId,
        quantity: Usd,
        ctx: &mut Context<Self>,
    ) -> Result<()> {
        let entry = self.setup_actors.entry(order_id);
        if matches!(entry, Entry::Occupied(ref occupied) if occupied.get().is_connected()) {
            bail!(
                "A contract setup for order id {} is already in progress",
                order_id
            )
        }

        let mut conn = self.db.acquire().await?;

        let current_order = load_order_by_id(order_id, &mut conn).await?;

        tracing::info!("Taking current order: {:?}", &current_order);

        let cfd = Cfd::new(
            current_order.clone(),
            quantity,
            CfdState::outgoing_order_request(),
        );

        insert_cfd_and_send_to_feed(&cfd, &mut conn, &self.projection_actor).await?;

        // Cleanup own order feed, after inserting the cfd.
        // Due to the 1:1 relationship between order and cfd we can never create another cfd for the
        // same order id.
        self.projection_actor.send(projection::Update(None)).await?;

        let announcement = self
            .oracle_actor
            .send(oracle::GetAnnouncement(cfd.order.oracle_event_id))
            .await?
            .with_context(|| format!("Announcement {} not found", cfd.order.oracle_event_id))?;

        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");
        let (addr, fut) = setup_taker::Actor::new(
            (current_order, quantity, self.n_payouts),
            (self.oracle_pk, announcement),
            &self.wallet,
            &self.wallet,
            self.conn_actor.clone(),
            &this,
            &this,
        )
        .create(None)
        .run();

        match entry {
            Entry::Occupied(mut disconnected) => {
                disconnected.insert(addr);
            }
            Entry::Vacant(vacant) => {
                vacant.insert(addr);
            }
        }

        self.tasks.add(fut);

        Ok(())
    }
}

impl<O, M, W> Actor<O, M, W>
where
    O: xtra::Handler<oracle::MonitorAttestation>,
    M: xtra::Handler<monitor::StartMonitoring>,
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle_setup_completed(&mut self, msg: Completed) -> Result<()> {
        let (order_id, dlc) = match msg {
            Completed::NewContract { order_id, dlc } => (order_id, dlc),
            Completed::Rejected { order_id } => {
                self.append_cfd_state_rejected(order_id).await?;
                return Ok(());
            }
            Completed::Failed { order_id, error } => {
                self.append_cfd_state_setup_failed(order_id, error).await?;
                return Ok(());
            }
        };

        let mut conn = self.db.acquire().await?;
        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        tracing::info!("Setup complete, publishing on chain now");

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
    }
}

impl<O: 'static, M: 'static, W: 'static> Actor<O, M, W>
where
    Self: xtra::Handler<CfdRollOverCompleted>,
    O: xtra::Handler<oracle::GetAnnouncement>,
    W: xtra::Handler<wallet::TryBroadcastTransaction>
        + xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::BuildPartyParams>,
{
    async fn handle_roll_over_accepted(
        &mut self,
        order_id: OrderId,
        oracle_event_id: BitMexPriceEventId,
        ctx: &mut Context<Self>,
    ) -> Result<()> {
        tracing::info!(%order_id, "Roll; over request got accepted");

        let (sender, receiver) = mpsc::unbounded();

        if let RollOverState::Active { .. } = self.roll_over_state {
            anyhow::bail!("Already rolling over a contract!")
        }

        let mut conn = self.db.acquire().await?;

        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;
        let dlc = cfd.open_dlc().context("CFD was in wrong state")?;

        let announcement = self
            .oracle_actor
            .send(oracle::GetAnnouncement(oracle_event_id))
            .await?
            .with_context(|| format!("Announcement {} not found", oracle_event_id))?;

        let contract_future = setup_contract::roll_over(
            xtra::message_channel::MessageChannel::sink(&self.conn_actor)
                .with(|msg| future::ok(wire::TakerToMaker::RollOverProtocol(msg))),
            receiver,
            (self.oracle_pk, announcement),
            RolloverParams::new(
                cfd.order.price,
                cfd.quantity_usd,
                cfd.order.leverage,
                cfd.refund_timelock_in_blocks(),
            ),
            Role::Taker,
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
            _task: task,
        };

        self.remove_pending_proposal(&order_id)
            .await
            .context("Could not remove accepted roll over")?;
        Ok(())
    }
}

impl<O: 'static, M: 'static, W: 'static> Actor<O, M, W>
where
    M: xtra::Handler<monitor::StartMonitoring>,
    O: xtra::Handler<oracle::MonitorAttestation>,
{
    async fn handle_roll_over_completed(
        &mut self,
        order_id: OrderId,
        dlc: Result<Dlc>,
    ) -> Result<()> {
        let dlc = dlc.context("Failed to roll over contract with maker")?;
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

impl<O: 'static, M: 'static, W: 'static> Actor<O, M, W>
where
    M: xtra::Handler<monitor::CollaborativeSettlement>,
    W: xtra::Handler<wallet::TryBroadcastTransaction>
        + xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::BuildPartyParams>,
{
    async fn handle_settlement_accepted(
        &mut self,
        order_id: OrderId,
        _ctx: &mut Context<Self>,
    ) -> Result<()> {
        tracing::info!(%order_id, "Settlement proposal got accepted");

        let mut conn = self.db.acquire().await?;

        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;
        let dlc = cfd.open_dlc().context("CFD was in wrong state")?;

        tracing::info!("DLC got loaded");

        let proposal = self.get_settlement_proposal(order_id)?;
        let (tx, sig_taker) = dlc.close_transaction(proposal)?;

        tracing::info!("found relevant closed transaction");

        // XXX: This is the call that does not return.
        // Possible deadlock
        #[allow(clippy::disallowed_method)]
        self.conn_actor
            .do_send_async(wire::TakerToMaker::InitiateSettlement {
                order_id,
                sig_taker,
            })
            .await?;

        tracing::info!("sent initiate settlement message");

        cfd.handle_proposal_signed(CollaborativeSettlement::new(
            tx.clone(),
            dlc.script_pubkey_for(cfd.role()),
            proposal.price,
        )?)?;

        tracing::info!("handle proposal signed");

        append_cfd_state(&cfd, &mut conn, &self.projection_actor).await?;

        self.remove_pending_proposal(&order_id).await?;

        tracing::info!("about to monitor for collab settlement");

        self.monitor_actor
            .send(monitor::CollaborativeSettlement {
                order_id,
                tx: (tx.txid(), dlc.script_pubkey_for(Role::Taker)),
            })
            .await?;

        Ok(())
    }
}

#[async_trait]
impl<O: 'static, M: 'static, W: 'static> Handler<TakeOffer> for Actor<O, M, W>
where
    Self: xtra::Handler<Completed>,
    O: xtra::Handler<oracle::GetAnnouncement> + xtra::Handler<oracle::MonitorAttestation>,
    W: xtra::Handler<wallet::BuildPartyParams> + xtra::Handler<wallet::Sign>,
{
    async fn handle(&mut self, msg: TakeOffer, ctx: &mut Context<Self>) -> Result<()> {
        self.handle_take_offer(msg.order_id, msg.quantity, ctx)
            .await
    }
}

#[async_trait]
impl<O: 'static, M: 'static, W: 'static> Handler<CfdAction> for Actor<O, M, W>
where
    W: xtra::Handler<wallet::TryBroadcastTransaction>
        + xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::BuildPartyParams>,
{
    async fn handle(&mut self, msg: CfdAction, _ctx: &mut Context<Self>) -> Result<()> {
        use CfdAction::*;

        if let Err(e) = match msg {
            Commit { order_id } => self.handle_commit(order_id).await,
            ProposeSettlement {
                order_id,
                current_price,
            } => {
                self.handle_propose_settlement(order_id, current_price)
                    .await
            }
            ProposeRollOver { order_id } => self.handle_propose_roll_over(order_id).await,
        } {
            tracing::error!("Message handler failed: {:#}", e);
            anyhow::bail!(e)
        }
        Ok(())
    }
}

#[async_trait]
impl<O: 'static, M: 'static, W: 'static> Handler<wire::MakerToTaker> for Actor<O, M, W>
where
    Self: xtra::Handler<CfdRollOverCompleted>,
    O: xtra::Handler<oracle::GetAnnouncement> + xtra::Handler<oracle::MonitorAttestation>,
    M: xtra::Handler<monitor::CollaborativeSettlement>,
    W: xtra::Handler<wallet::TryBroadcastTransaction>
        + xtra::Handler<wallet::Sign>
        + xtra::Handler<wallet::BuildPartyParams>,
{
    async fn handle(&mut self, msg: wire::MakerToTaker, ctx: &mut Context<Self>) {
        match msg {
            wire::MakerToTaker::CurrentOrder(current_order) => {
                log_error!(self.handle_new_order(current_order))
            }
            wire::MakerToTaker::ConfirmSettlement(order_id) => {
                log_error!(self.handle_settlement_accepted(order_id, ctx))
            }
            wire::MakerToTaker::RejectSettlement(order_id) => {
                log_error!(self.handle_settlement_rejected(order_id))
            }
            wire::MakerToTaker::InvalidOrderId(order_id) => {
                log_error!(self.handle_invalid_order_id(order_id))
            }
            wire::MakerToTaker::ConfirmRollOver {
                order_id,
                oracle_event_id,
            } => {
                log_error!(self.handle_roll_over_accepted(order_id, oracle_event_id, ctx))
            }
            wire::MakerToTaker::RejectRollOver(order_id) => {
                log_error!(self.handle_roll_over_rejected(order_id))
            }
            wire::MakerToTaker::RollOverProtocol(roll_over_msg) => {
                log_error!(self.handle_inc_roll_over_msg(roll_over_msg))
            }
            wire::MakerToTaker::Heartbeat => {
                unreachable!("Heartbeats should be handled somewhere else")
            }
            wire::MakerToTaker::ConfirmOrder(_)
            | wire::MakerToTaker::RejectOrder(_)
            | wire::MakerToTaker::Protocol { .. } => {
                unreachable!("These messages should be sent to the `setup_taker::Actor`")
            }
        }
    }
}

#[async_trait]
impl<O: 'static, M: 'static, W: 'static> Handler<Completed> for Actor<O, M, W>
where
    O: xtra::Handler<oracle::MonitorAttestation>,
    M: xtra::Handler<monitor::StartMonitoring>,
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle(&mut self, msg: Completed, _ctx: &mut Context<Self>) {
        log_error!(self.handle_setup_completed(msg))
    }
}

#[async_trait]
impl<O: 'static, M: 'static, W: 'static> Handler<CfdRollOverCompleted> for Actor<O, M, W>
where
    M: xtra::Handler<monitor::StartMonitoring>,
    O: xtra::Handler<oracle::MonitorAttestation>,
{
    async fn handle(&mut self, msg: CfdRollOverCompleted, _ctx: &mut Context<Self>) {
        log_error!(self.handle_roll_over_completed(msg.order_id, msg.dlc));
    }
}

#[async_trait]
impl<O: 'static, M: 'static, W: 'static> Handler<monitor::Event> for Actor<O, M, W>
where
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle(&mut self, msg: monitor::Event, _ctx: &mut Context<Self>) {
        log_error!(self.handle_monitoring_event(msg))
    }
}

#[async_trait]
impl<O: 'static, M: 'static, W: 'static> Handler<oracle::Attestation> for Actor<O, M, W>
where
    W: xtra::Handler<wallet::TryBroadcastTransaction>,
{
    async fn handle(&mut self, msg: oracle::Attestation, _ctx: &mut Context<Self>) {
        log_error!(self.handle_oracle_attestation(msg))
    }
}

#[async_trait]
impl<O: 'static, M: 'static, W: 'static> Handler<setup_taker::Started> for Actor<O, M, W> {
    async fn handle(&mut self, msg: setup_taker::Started, _ctx: &mut Context<Self>) {
        log_error!(self.handle_setup_started(msg.0))
    }
}

impl Message for TakeOffer {
    type Result = Result<()>;
}

impl Message for CfdAction {
    type Result = Result<()>;
}

impl Message for CfdRollOverCompleted {
    type Result = ();
}

impl<O: 'static, M: 'static, W: 'static> xtra::Actor for Actor<O, M, W> {}
