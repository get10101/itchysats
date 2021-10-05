use crate::actors::log_error;
use crate::db::{
    insert_cfd, insert_new_cfd_state_by_order_id, insert_order, load_all_cfds,
    load_cfd_by_order_id, load_order_by_id,
};
use crate::model::cfd::{
    Cfd, CfdState, CfdStateChangeEvent, CfdStateCommon, Dlc, Order, OrderId, Origin, Role,
};
use crate::model::Usd;
use crate::monitor::{self, MonitorParams};
use crate::wallet::Wallet;
use crate::wire::SetupMsg;
use crate::{oracle, send_to_socket, setup_contract, wire};
use anyhow::{Context as _, Result};
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::schnorrsig;
use futures::channel::mpsc;
use futures::{future, SinkExt};
use std::time::SystemTime;
use tokio::sync::watch;
use xtra::prelude::*;
use xtra::KeepRunning;

pub struct TakeOffer {
    pub order_id: OrderId,
    pub quantity: Usd,
}

pub struct ProposeSettlement {
    pub order_id: OrderId,
    pub current_price: Usd,
}

pub struct MakerStreamMessage {
    pub item: Result<wire::MakerToTaker>,
}

pub struct CfdSetupCompleted {
    pub order_id: OrderId,
    pub dlc: Result<Dlc>,
}

pub struct Commit {
    pub order_id: OrderId,
}

enum SetupState {
    Active {
        sender: mpsc::UnboundedSender<wire::SetupMsg>,
    },
    None,
}

pub struct Actor {
    db: sqlx::SqlitePool,
    wallet: Wallet,
    oracle_pk: schnorrsig::PublicKey,
    cfd_feed_actor_inbox: watch::Sender<Vec<Cfd>>,
    order_feed_actor_inbox: watch::Sender<Option<Order>>,
    send_to_maker: Address<send_to_socket::Actor<wire::TakerToMaker>>,
    monitor_actor: Address<monitor::Actor<Actor>>,
    setup_state: SetupState,
    latest_announcement: Option<oracle::Announcement>,
    _oracle_actor: Address<oracle::Actor<Actor, monitor::Actor<Actor>>>,
}

impl Actor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: sqlx::SqlitePool,
        wallet: Wallet,
        oracle_pk: schnorrsig::PublicKey,
        cfd_feed_actor_inbox: watch::Sender<Vec<Cfd>>,
        order_feed_actor_inbox: watch::Sender<Option<Order>>,
        send_to_maker: Address<send_to_socket::Actor<wire::TakerToMaker>>,
        monitor_actor: Address<monitor::Actor<Actor>>,
        oracle_actor: Address<oracle::Actor<Actor, monitor::Actor<Actor>>>,
    ) -> Self {
        Self {
            db,
            wallet,
            oracle_pk,
            cfd_feed_actor_inbox,
            order_feed_actor_inbox,
            send_to_maker,
            monitor_actor,
            setup_state: SetupState::None,
            latest_announcement: None,
            _oracle_actor: oracle_actor,
        }
    }

    async fn handle_take_offer(&mut self, order_id: OrderId, quantity: Usd) -> Result<()> {
        let mut conn = self.db.acquire().await?;

        let current_order = load_order_by_id(order_id, &mut conn).await?;

        tracing::info!("Taking current order: {:?}", &current_order);

        let cfd = Cfd::new(
            current_order.clone(),
            quantity,
            CfdState::OutgoingOrderRequest {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
            },
        );

        insert_cfd(cfd, &mut conn).await?;

        self.cfd_feed_actor_inbox
            .send(load_all_cfds(&mut conn).await?)?;
        self.send_to_maker
            .do_send_async(wire::TakerToMaker::TakeOrder { order_id, quantity })
            .await?;

        Ok(())
    }

    async fn handle_propose_settlement(
        &mut self,
        order_id: OrderId,
        current_price: Usd,
    ) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        let settlement = cfd.calculate_settlement(current_price)?;

        self.send_to_maker
            .do_send_async(wire::TakerToMaker::ProposeSettlement {
                order_id: settlement.order_id,
                timestamp: settlement.timestamp,
                taker: settlement.taker,
                maker: settlement.maker,
            })
            .await?;
        Ok(())
    }

    async fn handle_new_order(&mut self, order: Option<Order>) -> Result<()> {
        match order {
            Some(mut order) => {
                order.origin = Origin::Theirs;

                let mut conn = self.db.acquire().await?;
                insert_order(&order, &mut conn).await?;
                self.order_feed_actor_inbox.send(Some(order))?;
            }
            None => {
                self.order_feed_actor_inbox.send(None)?;
            }
        }
        Ok(())
    }

    async fn handle_order_accepted(
        &mut self,
        order_id: OrderId,
        ctx: &mut Context<Self>,
    ) -> Result<()> {
        tracing::info!(%order_id, "Order got accepted");

        let (sender, receiver) = mpsc::unbounded();

        if let SetupState::Active { .. } = self.setup_state {
            anyhow::bail!("Already setting up a contract!")
        }

        let mut conn = self.db.acquire().await?;
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

        self.cfd_feed_actor_inbox
            .send(load_all_cfds(&mut conn).await?)?;
        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        // let latest_announcement = self
        //     .latest_announcement
        //     .to_owned()
        //     .context("Unaware of oracle's latest announcement.")?;

        // self.oracle_actor
        //     .do_send_async(oracle::MonitorEvent {
        //         event_id: latest_announcement.id,
        //     })
        //     .await?;

        let nonce_pks = Vec::new();

        let contract_future = setup_contract::new(
            self.send_to_maker
                .clone()
                .into_sink()
                .with(|msg| future::ok(wire::TakerToMaker::Protocol(msg))),
            receiver,
            (self.oracle_pk, nonce_pks),
            cfd,
            self.wallet.clone(),
            Role::Taker,
        );

        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");

        tokio::spawn(async move {
            let dlc = contract_future.await;

            this.do_send_async(CfdSetupCompleted { order_id, dlc })
                .await
        });

        self.setup_state = SetupState::Active { sender };

        Ok(())
    }

    async fn handle_order_rejected(&mut self, order_id: OrderId) -> Result<()> {
        tracing::debug!(%order_id, "Order rejected");

        let mut conn = self.db.acquire().await?;
        insert_new_cfd_state_by_order_id(
            order_id,
            CfdState::Rejected {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
            },
            &mut conn,
        )
        .await?;

        self.cfd_feed_actor_inbox
            .send(load_all_cfds(&mut conn).await?)?;

        Ok(())
    }

    async fn handle_inc_protocol_msg(&mut self, msg: SetupMsg) -> Result<()> {
        match &mut self.setup_state {
            SetupState::Active { sender } => {
                sender.send(msg).await?;
            }
            SetupState::None => {
                anyhow::bail!("Received setup message without an active contract setup")
            }
        }

        Ok(())
    }

    async fn handle_cfd_setup_completed(
        &mut self,
        order_id: OrderId,
        dlc: Result<Dlc>,
    ) -> Result<()> {
        self.setup_state = SetupState::None;
        let dlc = dlc.context("Failed to setup contract with maker")?;

        tracing::info!("Setup complete, publishing on chain now");

        let mut conn = self.db.acquire().await?;

        insert_new_cfd_state_by_order_id(
            order_id,
            CfdState::PendingOpen {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
                dlc: dlc.clone(),
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

    async fn handle_monitoring_event(&mut self, event: monitor::Event) -> Result<()> {
        let order_id = event.order_id();

        let mut conn = self.db.acquire().await?;
        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        let new_state = cfd.handle(CfdStateChangeEvent::Monitor(event))?;

        insert_new_cfd_state_by_order_id(order_id, new_state.clone(), &mut conn).await?;

        self.cfd_feed_actor_inbox
            .send(load_all_cfds(&mut conn).await?)?;

        // TODO: Not sure that should be done here...
        //  Consider bubbling the refund availability up to the user, and let user trigger
        //  transaction publication
        if let CfdState::MustRefund { .. } = new_state {
            let signed_refund_tx = cfd.refund_tx()?;
            let txid = self
                .wallet
                .try_broadcast_transaction(signed_refund_tx)
                .await?;

            tracing::info!("Refund transaction published on chain: {}", txid);
        }

        Ok(())
    }

    // TODO: Duplicated with maker
    async fn handle_commit(&mut self, order_id: OrderId) -> Result<()> {
        let mut conn = self.db.acquire().await?;
        let mut cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        let signed_commit_tx = cfd.commit_tx()?;

        let txid = self
            .wallet
            .try_broadcast_transaction(signed_commit_tx)
            .await?;

        tracing::info!("Commit transaction published on chain: {}", txid);

        let new_state = cfd.handle(CfdStateChangeEvent::CommitTxSent)?;
        insert_new_cfd_state_by_order_id(cfd.order.id, new_state, &mut conn).await?;

        self.cfd_feed_actor_inbox
            .send(load_all_cfds(&mut conn).await?)?;

        Ok(())
    }

    async fn handle_oracle_announcements(
        &mut self,
        announcements: oracle::Announcements,
    ) -> Result<()> {
        tracing::debug!("Updating latest oracle announcements");
        self.latest_announcement = Some(announcements.0.last().unwrap().clone());

        Ok(())
    }

    async fn handle_oracle_attestation(&mut self, attestation: oracle::Attestation) -> Result<()> {
        tracing::debug!(
            "Learnt latest oracle attestation for event: {}",
            attestation.id
        );

        todo!(
            "Update all CFDs which care about this particular attestation, based on the event ID"
        );
    }
}

#[async_trait]
impl Handler<TakeOffer> for Actor {
    async fn handle(&mut self, msg: TakeOffer, _ctx: &mut Context<Self>) {
        log_error!(self.handle_take_offer(msg.order_id, msg.quantity));
    }
}

#[async_trait]
impl Handler<ProposeSettlement> for Actor {
    async fn handle(&mut self, msg: ProposeSettlement, _ctx: &mut Context<Self>) {
        log_error!(self.handle_propose_settlement(msg.order_id, msg.current_price));
    }
}

#[async_trait]
impl Handler<MakerStreamMessage> for Actor {
    async fn handle(
        &mut self,
        message: MakerStreamMessage,
        ctx: &mut Context<Self>,
    ) -> KeepRunning {
        let msg = match message.item {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!("Error while receiving message from maker: {:#}", e);
                return KeepRunning::Yes;
            }
        };

        match msg {
            wire::MakerToTaker::CurrentOrder(current_order) => {
                log_error!(self.handle_new_order(current_order))
            }
            wire::MakerToTaker::ConfirmOrder(order_id) => {
                log_error!(self.handle_order_accepted(order_id, ctx))
            }
            wire::MakerToTaker::RejectOrder(order_id) => {
                log_error!(self.handle_order_rejected(order_id))
            }
            wire::MakerToTaker::InvalidOrderId(_) => todo!(),
            wire::MakerToTaker::Protocol(setup_msg) => {
                log_error!(self.handle_inc_protocol_msg(setup_msg))
            }
        }

        KeepRunning::Yes
    }
}

#[async_trait]
impl Handler<CfdSetupCompleted> for Actor {
    async fn handle(&mut self, msg: CfdSetupCompleted, _ctx: &mut Context<Self>) {
        log_error!(self.handle_cfd_setup_completed(msg.order_id, msg.dlc));
    }
}

#[async_trait]
impl Handler<monitor::Event> for Actor {
    async fn handle(&mut self, msg: monitor::Event, _ctx: &mut Context<Self>) {
        log_error!(self.handle_monitoring_event(msg))
    }
}

#[async_trait]
impl Handler<oracle::Announcements> for Actor {
    async fn handle(&mut self, msg: oracle::Announcements, _ctx: &mut Context<Self>) {
        log_error!(self.handle_oracle_announcements(msg))
    }
}

#[async_trait]
impl Handler<oracle::Attestation> for Actor {
    async fn handle(&mut self, msg: oracle::Attestation, _ctx: &mut Context<Self>) {
        log_error!(self.handle_oracle_attestation(msg))
    }
}

#[async_trait]
impl Handler<Commit> for Actor {
    async fn handle(&mut self, msg: Commit, _ctx: &mut Context<Self>) {
        log_error!(self.handle_commit(msg.order_id))
    }
}

impl Message for TakeOffer {
    type Result = ();
}

impl Message for ProposeSettlement {
    type Result = ();
}

// this signature is a bit different because we use `Address::attach_stream`
impl Message for MakerStreamMessage {
    type Result = KeepRunning;
}

impl Message for CfdSetupCompleted {
    type Result = ();
}

impl Message for Commit {
    type Result = ();
}

impl xtra::Actor for Actor {}
