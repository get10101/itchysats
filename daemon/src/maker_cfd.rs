use crate::actors::log_error;
use crate::db::{
    insert_cfd, insert_new_cfd_state_by_order_id, insert_order, load_all_cfds,
    load_cfd_by_order_id, load_cfds_by_oracle_event_id, load_order_by_id,
};
use crate::maker_inc_connections::TakerCommand;
use crate::model::cfd::{
    Cfd, CfdState, CfdStateChangeEvent, CfdStateCommon, Dlc, Order, OrderId, Origin, Role,
    RollOverProposal, SettlementKind, SettlementProposal, UpdateCfdProposal, UpdateCfdProposals,
};
use crate::model::{TakerId, Usd};
use crate::monitor::MonitorParams;
use crate::wallet::Wallet;
use crate::{maker_inc_connections, monitor, oracle, setup_contract, wire};
use anyhow::{bail, Context as _, Result};
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::schnorrsig;
use futures::channel::mpsc;
use futures::{future, SinkExt};
use std::collections::HashMap;
use std::time::SystemTime;
use tokio::sync::watch;
use xtra::prelude::*;
use xtra::KeepRunning;

pub struct AcceptOrder {
    pub order_id: OrderId,
}

pub struct RejectOrder {
    pub order_id: OrderId,
}

pub struct Commit {
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

pub struct NewOrder {
    pub price: Usd,
    pub min_quantity: Usd,
    pub max_quantity: Usd,
}

pub struct NewTakerOnline {
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

pub struct TakerStreamMessage {
    pub taker_id: TakerId,
    pub item: Result<wire::TakerToMaker>,
}

pub struct Actor {
    db: sqlx::SqlitePool,
    wallet: Wallet,
    oracle_pk: schnorrsig::PublicKey,
    cfd_feed_actor_inbox: watch::Sender<Vec<Cfd>>,
    order_feed_sender: watch::Sender<Option<Order>>,
    update_cfd_feed_sender: watch::Sender<UpdateCfdProposals>,
    takers: Address<maker_inc_connections::Actor>,
    current_order_id: Option<OrderId>,
    monitor_actor: Address<monitor::Actor<Actor>>,
    setup_state: SetupState,
    roll_over_state: RollOverState,
    oracle_actor: Address<oracle::Actor<Actor, monitor::Actor<Actor>>>,
    // Maker needs to also store TakerId to be able to send a reply back
    current_pending_proposals: HashMap<OrderId, (UpdateCfdProposal, TakerId)>,
}

enum SetupState {
    Active {
        taker: TakerId,
        sender: mpsc::UnboundedSender<wire::SetupMsg>,
    },
    None,
}

enum RollOverState {
    Active {
        taker: TakerId,
        sender: mpsc::UnboundedSender<wire::RollOverMsg>,
    },
    None,
}

impl Actor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: sqlx::SqlitePool,
        wallet: Wallet,
        oracle_pk: schnorrsig::PublicKey,
        cfd_feed_actor_inbox: watch::Sender<Vec<Cfd>>,
        order_feed_sender: watch::Sender<Option<Order>>,
        update_cfd_feed_sender: watch::Sender<UpdateCfdProposals>,
        takers: Address<maker_inc_connections::Actor>,
        monitor_actor: Address<monitor::Actor<Actor>>,
        oracle_actor: Address<oracle::Actor<Actor, monitor::Actor<Actor>>>,
    ) -> Self {
        Self {
            db,
            wallet,
            oracle_pk,
            cfd_feed_actor_inbox,
            order_feed_sender,
            update_cfd_feed_sender,
            takers,
            current_order_id: None,
            monitor_actor,
            setup_state: SetupState::None,
            roll_over_state: RollOverState::None,
            oracle_actor,
            current_pending_proposals: HashMap::new(),
        }
    }

    /// Send pending proposals for the purposes of UI updates.
    /// Filters out the TakerIds, as they are an implementation detail inside of
    /// the actor
    fn send_pending_proposals(&self) -> Result<()> {
        Ok(self.update_cfd_feed_sender.send(
            self.current_pending_proposals
                .iter()
                .map(|(order_id, (update_cfd, _))| (*order_id, (update_cfd.clone())))
                .collect(),
        )?)
    }

    fn get_taker_id_of_proposal(&self, order_id: &OrderId) -> Result<TakerId> {
        let (_, taker_id) = self
            .current_pending_proposals
            .get(order_id)
            .context("Could not find proposal for given order id")?;
        Ok(*taker_id)
    }

    /// Removes a proposal and updates the update cfd proposals' feed
    fn remove_pending_proposal(&mut self, order_id: &OrderId) -> Result<()> {
        if self.current_pending_proposals.remove(order_id).is_none() {
            anyhow::bail!("Could not find proposal with order id: {}", &order_id)
        }
        self.send_pending_proposals()?;
        Ok(())
    }

    async fn handle_new_order(
        &mut self,
        price: Usd,
        min_quantity: Usd,
        max_quantity: Usd,
    ) -> Result<()> {
        let oracle_event_id =
            oracle::next_announcement_after(time::OffsetDateTime::now_utc() + Order::TERM);

        let order = Order::new(
            price,
            min_quantity,
            max_quantity,
            Origin::Ours,
            oracle_event_id,
        )?;

        // 1. Save to DB
        let mut conn = self.db.acquire().await?;
        insert_order(&order, &mut conn).await?;

        // 2. Update actor state to current order
        self.current_order_id.replace(order.id);

        // 3. Notify UI via feed
        self.order_feed_sender.send(Some(order.clone()))?;

        // 4. Inform connected takers
        self.takers
            .do_send_async(maker_inc_connections::BroadcastOrder(Some(order)))
            .await?;
        Ok(())
    }

    async fn handle_new_taker_online(&mut self, taker_id: TakerId) -> Result<()> {
        let mut conn = self.db.acquire().await?;

        let current_order = match self.current_order_id {
            Some(current_order_id) => Some(load_order_by_id(current_order_id, &mut conn).await?),
            None => None,
        };

        self.takers
            .do_send_async(maker_inc_connections::TakerMessage {
                taker_id,
                command: TakerCommand::SendOrder {
                    order: current_order,
                },
            })
            .await?;

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
        self.send_pending_proposals()?;

        Ok(())
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
        self.send_pending_proposals()?;

        Ok(())
    }

    async fn handle_inc_protocol_msg(
        &mut self,
        taker_id: TakerId,
        msg: wire::SetupMsg,
    ) -> Result<()> {
        match &mut self.setup_state {
            SetupState::Active { taker, sender } if taker_id == *taker => {
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
            RollOverState::Active { taker, sender } if taker_id == *taker => {
                sender.send(msg).await?;
            }
            RollOverState::Active { taker, .. } => {
                anyhow::bail!("Currently rolling over with different taker {}", taker)
            }
            RollOverState::None => {}
        }

        Ok(())
    }

    async fn handle_cfd_setup_completed(
        &mut self,
        order_id: OrderId,
        dlc: Result<Dlc>,
    ) -> Result<()> {
        self.setup_state = SetupState::None;

        let dlc = dlc.context("Failed to setup contract with taker")?;

        let mut conn = self.db.acquire().await?;

        insert_new_cfd_state_by_order_id(
            order_id,
            CfdState::PendingOpen {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
                dlc: dlc.clone(),
                attestation: None,
            },
            &mut conn,
        )
        .await?;

        self.cfd_feed_actor_inbox
            .send(load_all_cfds(&mut conn).await?)?;

        let txid = self
            .wallet
            .try_broadcast_transaction(dlc.lock.0.clone())
            .await?;

        tracing::info!("Lock transaction published with txid {}", txid);

        // TODO: It's a bit suspicious to load this just to get the
        // refund timelock
        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        self.monitor_actor
            .do_send_async(monitor::StartMonitoring {
                id: order_id,
                params: MonitorParams::from_dlc_and_timelocks(dlc, cfd.refund_timelock_in_blocks()),
            })
            .await?;

        Ok(())
    }

    async fn handle_cfd_roll_over_completed(
        &mut self,
        order_id: OrderId,
        dlc: Result<Dlc>,
    ) -> Result<()> {
        let dlc = dlc.context("Failed to roll over contract with taker")?;
        self.roll_over_state = RollOverState::None;

        let mut conn = self.db.acquire().await?;
        insert_new_cfd_state_by_order_id(
            order_id,
            CfdState::Open {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
                dlc: dlc.clone(),
                attestation: None,
            },
            &mut conn,
        )
        .await?;

        self.cfd_feed_actor_inbox
            .send(load_all_cfds(&mut conn).await?)?;

        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        self.monitor_actor
            .do_send_async(monitor::StartMonitoring {
                id: order_id,
                params: MonitorParams::from_dlc_and_timelocks(dlc, cfd.refund_timelock_in_blocks()),
            })
            .await?;

        Ok(())
    }

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
                self.takers
                    .do_send_async(maker_inc_connections::TakerMessage {
                        taker_id,
                        command: TakerCommand::NotifyInvalidOrderId { id: order_id },
                    })
                    .await?;
                // TODO: Return an error here?
                return Ok(());
            }
        };

        // 2. Insert CFD in DB
        let cfd = Cfd::new(
            current_order.clone(),
            quantity,
            CfdState::IncomingOrderRequest {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
                taker_id,
            },
        );
        insert_cfd(cfd, &mut conn).await?;

        self.cfd_feed_actor_inbox
            .send(load_all_cfds(&mut conn).await?)?;

        // 3. Remove current order
        self.current_order_id = None;
        self.takers
            .do_send_async(maker_inc_connections::BroadcastOrder(None))
            .await?;
        self.order_feed_sender.send(None)?;

        Ok(())
    }

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

        // Validate if order is still valid
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

        let (sender, receiver) = mpsc::unbounded();

        insert_new_cfd_state_by_order_id(
            order_id,
            CfdState::ContractSetup {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
            },
            &mut conn,
        )
        .await?;

        // use `.send` here to ensure we only continue once the message has been sent
        self.takers
            .send(maker_inc_connections::TakerMessage {
                taker_id,
                command: TakerCommand::NotifyOrderAccepted { id: order_id },
            })
            .await?;

        self.cfd_feed_actor_inbox
            .send(load_all_cfds(&mut conn).await?)?;

        let offer_announcement = self
            .oracle_actor
            .send(oracle::GetAnnouncement(cfd.order.oracle_event_id.clone()))
            .await?
            .with_context(|| format!("Announcement {} not found", cfd.order.oracle_event_id))?;

        self.oracle_actor
            .do_send_async(oracle::MonitorEvent {
                event_id: offer_announcement.id.clone(),
            })
            .await?;

        let contract_future = setup_contract::new(
            self.takers.clone().into_sink().with(move |msg| {
                future::ok(maker_inc_connections::TakerMessage {
                    taker_id,
                    command: TakerCommand::Protocol(msg),
                })
            }),
            receiver,
            (self.oracle_pk, offer_announcement.clone().into()),
            cfd,
            self.wallet.clone(),
            Role::Maker,
        );

        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");

        tokio::spawn(async move {
            let dlc = contract_future.await;

            this.do_send_async(CfdSetupCompleted { order_id, dlc })
                .await
        });

        self.setup_state = SetupState::Active {
            sender,
            taker: taker_id,
        };

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

        // Update order in db
        insert_new_cfd_state_by_order_id(
            order_id,
            CfdState::Rejected {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
            },
            &mut conn,
        )
        .await
        .unwrap();

        self.takers
            .do_send_async(maker_inc_connections::TakerMessage {
                taker_id,
                command: TakerCommand::NotifyOrderRejected { id: order_id },
            })
            .await?;
        self.cfd_feed_actor_inbox
            .send(load_all_cfds(&mut conn).await?)?;

        // Remove order for all
        self.current_order_id = None;
        self.takers
            .do_send_async(maker_inc_connections::BroadcastOrder(None))
            .await?;
        self.order_feed_sender.send(None)?;

        Ok(())
    }

    async fn handle_commit(&mut self, order_id: OrderId) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        let signed_commit_tx = cfd.commit_tx()?;

        let txid = self
            .wallet
            .try_broadcast_transaction(signed_commit_tx)
            .await?;

        tracing::info!("Commit transaction published on chain: {}", txid);

        if let Some(new_state) = cfd.handle(CfdStateChangeEvent::CommitTxSent)? {
            insert_new_cfd_state_by_order_id(cfd.order.id, new_state, &mut conn).await?;
        }

        self.cfd_feed_actor_inbox
            .send(load_all_cfds(&mut conn).await?)?;
        Ok(())
    }

    async fn handle_accept_settlement(&mut self, order_id: OrderId) -> Result<()> {
        tracing::debug!(%order_id, "Maker accepts a settlement proposal" );

        let taker_id = self.get_taker_id_of_proposal(&order_id)?;

        // TODO: Initiate the settlement - should we start calculating the
        // signature here?

        self.takers
            .do_send_async(maker_inc_connections::TakerMessage {
                taker_id,
                command: TakerCommand::NotifySettlementAccepted { id: order_id },
            })
            .await?;

        self.remove_pending_proposal(&order_id)
            .context("accepted settlement")?;
        Ok(())
    }

    async fn handle_reject_settlement(&mut self, order_id: OrderId) -> Result<()> {
        tracing::debug!(%order_id, "Maker rejects a settlement proposal" );

        let taker_id = self.get_taker_id_of_proposal(&order_id)?;

        self.takers
            .do_send_async(maker_inc_connections::TakerMessage {
                taker_id,
                command: TakerCommand::NotifySettlementRejected { id: order_id },
            })
            .await?;

        self.remove_pending_proposal(&order_id)
            .context("rejected settlement")?;
        Ok(())
    }

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

        // TODO: do we want to store in the db that we rolled over?

        let oracle_event_id =
            oracle::next_announcement_after(time::OffsetDateTime::now_utc() + Order::TERM);
        let announcement = self
            .oracle_actor
            .send(oracle::GetAnnouncement(oracle_event_id.clone()))
            .await?
            .with_context(|| format!("Announcement {} not found", oracle_event_id))?;

        self.takers
            .send(maker_inc_connections::TakerMessage {
                taker_id,
                command: TakerCommand::NotifyRollOverAccepted {
                    id: proposal.order_id,
                    oracle_event_id,
                },
            })
            .await?;

        self.oracle_actor
            .do_send_async(oracle::MonitorEvent {
                event_id: announcement.id.clone(),
            })
            .await?;

        let (sender, receiver) = mpsc::unbounded();
        let contract_future = setup_contract::roll_over(
            self.takers.clone().into_sink().with(move |msg| {
                future::ok(maker_inc_connections::TakerMessage {
                    taker_id,
                    command: TakerCommand::RollOverProtocol(msg),
                })
            }),
            receiver,
            (self.oracle_pk, announcement),
            cfd,
            Role::Maker,
            dlc,
        );

        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");

        self.roll_over_state = RollOverState::Active {
            sender,
            taker: taker_id,
        };

        tokio::spawn(async move {
            let dlc = contract_future.await;

            this.do_send_async(CfdRollOverCompleted { order_id, dlc })
                .await
        });

        self.remove_pending_proposal(&order_id)
            .context("accepted roll_over")?;
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

        self.takers
            .do_send_async(maker_inc_connections::TakerMessage {
                taker_id,
                command: TakerCommand::NotifyRollOverRejected { id: order_id },
            })
            .await?;

        self.remove_pending_proposal(&order_id)
            .context("rejected roll_over")?;
        Ok(())
    }

    async fn handle_monitoring_event(&mut self, event: monitor::Event) -> Result<()> {
        let order_id = event.order_id();

        let mut conn = self.db.acquire().await?;
        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        if cfd.handle(CfdStateChangeEvent::Monitor(event))?.is_none() {
            // early exit if there was not state change
            // this is for cases where we are already in a final state
            return Ok(());
        }

        insert_new_cfd_state_by_order_id(order_id, cfd.state.clone(), &mut conn).await?;

        self.cfd_feed_actor_inbox
            .send(load_all_cfds(&mut conn).await?)?;

        // TODO: code duplication maker/taker
        if let CfdState::OpenCommitted { .. } = cfd.state {
            self.try_cet_publication(cfd).await?;
        } else if let CfdState::MustRefund { .. } = cfd.state {
            let signed_refund_tx = cfd.refund_tx()?;
            let txid = self
                .wallet
                .try_broadcast_transaction(signed_refund_tx)
                .await?;

            tracing::info!("Refund transaction published on chain: {}", txid);
        }

        Ok(())
    }

    async fn handle_oracle_attestation(&mut self, attestation: oracle::Attestation) -> Result<()> {
        tracing::debug!(
            "Learnt latest oracle attestation for event: {}",
            attestation.id
        );

        let mut conn = self.db.acquire().await?;
        let cfds = load_cfds_by_oracle_event_id(attestation.id.clone(), &mut conn).await?;

        for mut cfd in cfds {
            if cfd
                .handle(CfdStateChangeEvent::OracleAttestation(
                    attestation.clone().into(),
                ))?
                .is_none()
            {
                // if we don't transition to a new state after oracle attestation we ignore the cfd
                // this is for cases where we cannot handle the attestation which should be in a
                // final state
                continue;
            }

            insert_new_cfd_state_by_order_id(cfd.order.id, cfd.state.clone(), &mut conn).await?;
            self.cfd_feed_actor_inbox
                .send(load_all_cfds(&mut conn).await?)?;

            if let Err(e) = self.try_cet_publication(cfd).await {
                tracing::error!("Error when trying to publish CET: {:#}", e);
                continue;
            }
        }

        Ok(())
    }

    // TODO: code duplication maker/taker
    async fn try_cet_publication(&mut self, mut cfd: Cfd) -> Result<()> {
        let mut conn = self.db.acquire().await?;

        match cfd.cet()? {
            Ok(cet) => {
                let txid = self.wallet.try_broadcast_transaction(cet).await?;
                tracing::info!("CET published with txid {}", txid);

                if cfd.handle(CfdStateChangeEvent::CetSent)?.is_none() {
                    bail!("If we can get the CET we should be able to transition")
                }

                insert_new_cfd_state_by_order_id(cfd.order.id, cfd.state.clone(), &mut conn)
                    .await?;

                self.cfd_feed_actor_inbox
                    .send(load_all_cfds(&mut conn).await?)?;
            }
            Err(not_ready_yet) => {
                tracing::debug!("{:#}", not_ready_yet);
                return Ok(());
            }
        };

        Ok(())
    }
}

#[async_trait]
impl Handler<AcceptOrder> for Actor {
    async fn handle(&mut self, msg: AcceptOrder, ctx: &mut Context<Self>) {
        log_error!(self.handle_accept_order(msg.order_id, ctx))
    }
}

#[async_trait]
impl Handler<RejectOrder> for Actor {
    async fn handle(&mut self, msg: RejectOrder, _ctx: &mut Context<Self>) {
        log_error!(self.handle_reject_order(msg.order_id))
    }
}

#[async_trait]
impl Handler<AcceptSettlement> for Actor {
    async fn handle(&mut self, msg: AcceptSettlement, _ctx: &mut Context<Self>) {
        log_error!(self.handle_accept_settlement(msg.order_id))
    }
}

#[async_trait]
impl Handler<RejectSettlement> for Actor {
    async fn handle(&mut self, msg: RejectSettlement, _ctx: &mut Context<Self>) {
        log_error!(self.handle_reject_settlement(msg.order_id))
    }
}

#[async_trait]
impl Handler<AcceptRollOver> for Actor {
    async fn handle(&mut self, msg: AcceptRollOver, ctx: &mut Context<Self>) {
        log_error!(self.handle_accept_roll_over(msg.order_id, ctx))
    }
}

#[async_trait]
impl Handler<RejectRollOver> for Actor {
    async fn handle(&mut self, msg: RejectRollOver, _ctx: &mut Context<Self>) {
        log_error!(self.handle_reject_roll_over(msg.order_id))
    }
}

#[async_trait]
impl Handler<Commit> for Actor {
    async fn handle(&mut self, msg: Commit, _ctx: &mut Context<Self>) {
        log_error!(self.handle_commit(msg.order_id))
    }
}

#[async_trait]
impl Handler<NewOrder> for Actor {
    async fn handle(&mut self, msg: NewOrder, _ctx: &mut Context<Self>) {
        log_error!(self.handle_new_order(msg.price, msg.min_quantity, msg.max_quantity));
    }
}

#[async_trait]
impl Handler<NewTakerOnline> for Actor {
    async fn handle(&mut self, msg: NewTakerOnline, _ctx: &mut Context<Self>) {
        log_error!(self.handle_new_taker_online(msg.id));
    }
}

#[async_trait]
impl Handler<CfdSetupCompleted> for Actor {
    async fn handle(&mut self, msg: CfdSetupCompleted, _ctx: &mut Context<Self>) {
        log_error!(self.handle_cfd_setup_completed(msg.order_id, msg.dlc));
    }
}

#[async_trait]
impl Handler<CfdRollOverCompleted> for Actor {
    async fn handle(&mut self, msg: CfdRollOverCompleted, _ctx: &mut Context<Self>) {
        log_error!(self.handle_cfd_roll_over_completed(msg.order_id, msg.dlc));
    }
}

#[async_trait]
impl Handler<monitor::Event> for Actor {
    async fn handle(&mut self, msg: monitor::Event, _ctx: &mut Context<Self>) {
        log_error!(self.handle_monitoring_event(msg))
    }
}

#[async_trait]
impl Handler<TakerStreamMessage> for Actor {
    async fn handle(&mut self, msg: TakerStreamMessage, _ctx: &mut Context<Self>) -> KeepRunning {
        let TakerStreamMessage { taker_id, item } = msg;
        let msg = match item {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!(
                    "Error while receiving message from taker {}: {:#}",
                    taker_id,
                    e
                );
                return KeepRunning::Yes;
            }
        };

        match msg {
            wire::TakerToMaker::TakeOrder { order_id, quantity } => {
                log_error!(self.handle_take_order(taker_id, order_id, quantity))
            }
            wire::TakerToMaker::ProposeSettlement {
                order_id,
                timestamp,
                taker,
                maker,
            } => {
                log_error!(self.handle_propose_settlement(
                    taker_id,
                    SettlementProposal {
                        order_id,
                        timestamp,
                        taker,
                        maker
                    }
                ))
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

        KeepRunning::Yes
    }
}

#[async_trait]
impl Handler<oracle::Attestation> for Actor {
    async fn handle(&mut self, msg: oracle::Attestation, _ctx: &mut Context<Self>) {
        log_error!(self.handle_oracle_attestation(msg))
    }
}

impl Message for NewOrder {
    type Result = ();
}

impl Message for NewTakerOnline {
    type Result = ();
}

impl Message for CfdSetupCompleted {
    type Result = ();
}

impl Message for CfdRollOverCompleted {
    type Result = ();
}

impl Message for AcceptOrder {
    type Result = ();
}

impl Message for RejectOrder {
    type Result = ();
}

impl Message for Commit {
    type Result = ();
}

impl Message for AcceptSettlement {
    type Result = ();
}

impl Message for RejectSettlement {
    type Result = ();
}

impl Message for AcceptRollOver {
    type Result = ();
}

impl Message for RejectRollOver {
    type Result = ();
}

// this signature is a bit different because we use `Address::attach_stream`
impl Message for TakerStreamMessage {
    type Result = KeepRunning;
}

impl xtra::Actor for Actor {}
