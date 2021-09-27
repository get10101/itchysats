use crate::actors::log_error;
use crate::db::{
    insert_cfd, insert_new_cfd_state_by_order_id, insert_order, load_all_cfds,
    load_cfd_by_order_id, load_order_by_id,
};
use crate::maker_inc_connections::TakerCommand;
use crate::model::cfd::{Cfd, CfdState, CfdStateChangeEvent, CfdStateCommon, Dlc, Order, OrderId};
use crate::model::{TakerId, Usd};
use crate::monitor::MonitorParams;
use crate::wallet::Wallet;
use crate::wire::{self, SetupMsg};
use crate::{maker_inc_connections, monitor, setup_contract_actor};
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::schnorrsig;
use std::time::SystemTime;
use tokio::sync::{mpsc, watch};
use xtra::prelude::*;
use xtra::KeepRunning;

pub struct AcceptOrder {
    pub order_id: OrderId,
}

pub struct RejectOrder {
    pub order_id: OrderId,
}

pub struct NewOrder(pub Order);

pub struct NewTakerOnline {
    pub id: TakerId,
}

pub struct CfdSetupCompleted {
    pub order_id: OrderId,
    pub dlc: Dlc,
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
    takers: Address<maker_inc_connections::Actor>,
    current_order_id: Option<OrderId>,
    current_contract_setup: Option<mpsc::UnboundedSender<SetupMsg>>,
    // TODO: Move the contract setup into a dedicated actor and send messages to that actor that
    // manages the state instead of this ugly buffer
    contract_setup_message_buffer: Vec<SetupMsg>,
    monitor_actor: Address<monitor::Actor<Actor>>,
}

impl Actor {
    pub async fn new(
        db: sqlx::SqlitePool,
        wallet: Wallet,
        oracle_pk: schnorrsig::PublicKey,
        cfd_feed_actor_inbox: watch::Sender<Vec<Cfd>>,
        order_feed_sender: watch::Sender<Option<Order>>,
        takers: Address<maker_inc_connections::Actor>,
        monitor_actor: Address<monitor::Actor<Actor>>,
    ) -> Result<Self> {
        let mut conn = db.acquire().await?;

        // populate the CFD feed with existing CFDs
        cfd_feed_actor_inbox.send(load_all_cfds(&mut conn).await?)?;

        Ok(Self {
            db,
            wallet,
            oracle_pk,
            cfd_feed_actor_inbox,
            order_feed_sender,
            takers,
            current_order_id: None,
            current_contract_setup: None,
            contract_setup_message_buffer: vec![],
            monitor_actor,
        })
    }

    async fn handle_new_order(&mut self, msg: NewOrder) -> Result<()> {
        let order = msg.0;

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

    async fn handle_new_taker_online(&mut self, msg: NewTakerOnline) -> Result<()> {
        let mut conn = self.db.acquire().await?;

        let current_order = match self.current_order_id {
            Some(current_order_id) => Some(load_order_by_id(current_order_id, &mut conn).await?),
            None => None,
        };

        self.takers
            .do_send_async(maker_inc_connections::TakerMessage {
                taker_id: msg.id,
                command: TakerCommand::SendOrder {
                    order: current_order,
                },
            })
            .await?;

        Ok(())
    }

    async fn handle_inc_protocol_msg(
        &mut self,
        _taker_id: TakerId,
        msg: wire::SetupMsg,
    ) -> Result<()> {
        let inbox = match &self.current_contract_setup {
            None => {
                self.contract_setup_message_buffer.push(msg);
                return Ok(());
            }
            Some(inbox) => inbox,
        };
        inbox.send(msg)?;

        Ok(())
    }

    async fn handle_cfd_setup_completed(&mut self, msg: CfdSetupCompleted) -> Result<()> {
        let mut conn = self.db.acquire().await?;

        self.current_contract_setup = None;
        self.contract_setup_message_buffer = vec![];

        insert_new_cfd_state_by_order_id(
            msg.order_id,
            CfdState::PendingOpen {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
                dlc: msg.dlc.clone(),
            },
            &mut conn,
        )
        .await?;

        self.cfd_feed_actor_inbox
            .send(load_all_cfds(&mut conn).await?)?;

        let txid = self
            .wallet
            .try_broadcast_transaction(msg.dlc.lock.0.clone())
            .await?;

        tracing::info!("Lock transaction published with txid {}", txid);

        // TODO: It's a bit suspicious to load this just to get the
        // refund timelock
        let cfd = load_cfd_by_order_id(msg.order_id, &mut conn).await?;

        let script_pubkey = msg.dlc.address.script_pubkey();
        self.monitor_actor
            .do_send_async(monitor::StartMonitoring {
                id: msg.order_id,
                params: MonitorParams {
                    lock: (msg.dlc.lock.0.txid(), msg.dlc.lock.1),
                    commit: (msg.dlc.commit.0.txid(), msg.dlc.commit.2),
                    cets: msg
                        .dlc
                        .cets
                        .into_iter()
                        .map(|(tx, _, range)| (tx.txid(), script_pubkey.clone(), range))
                        .collect(),
                    refund: (
                        msg.dlc.refund.0.txid(),
                        script_pubkey,
                        cfd.refund_timelock_in_blocks(),
                    ),
                },
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
        msg: AcceptOrder,
        ctx: &mut Context<Self>,
    ) -> Result<()> {
        tracing::debug!(%msg.order_id, "Maker accepts an order" );

        let mut conn = self.db.acquire().await?;

        // Validate if order is still valid
        let cfd = load_cfd_by_order_id(msg.order_id, &mut conn).await?;

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
        let order_id = cfd.order.id;
        insert_new_cfd_state_by_order_id(
            order_id,
            CfdState::Accepted {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
            },
            &mut conn,
        )
        .await
        .unwrap();

        // use `.send` here to ensure we only continue once the message has been sent
        self.takers
            .send(maker_inc_connections::TakerMessage {
                taker_id,
                command: TakerCommand::NotifyOrderAccepted { id: msg.order_id },
            })
            .await?;

        self.cfd_feed_actor_inbox
            .send(load_all_cfds(&mut conn).await?)?;

        // Start contract setup
        tracing::info!("Starting contract setup");

        // Kick-off the CFD protocol
        let (sk, pk) = crate::keypair::new(&mut rand::thread_rng());

        let margin = cfd.margin()?;

        let maker_params = self.wallet.build_party_params(margin, pk).await?;

        let (actor, inbox) = setup_contract_actor::new(
            {
                let inbox = self.takers.clone();
                move |msg| {
                    tokio::spawn(inbox.do_send_async(maker_inc_connections::TakerMessage {
                        taker_id,
                        command: TakerCommand::OutProtocolMsg { setup_msg: msg },
                    }));
                }
            },
            setup_contract_actor::OwnParams::Maker(maker_params),
            sk,
            self.oracle_pk,
            cfd,
            self.wallet.clone(),
        );

        self.current_contract_setup.replace(inbox.clone());

        for msg in self.contract_setup_message_buffer.drain(..) {
            inbox.send(msg)?;
        }

        // TODO: Should we do this here or already earlier or after the spawn?
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

        let address = ctx
            .address()
            .expect("actor to be able to give address to itself");

        tokio::spawn(async move {
            address
                .do_send_async(CfdSetupCompleted {
                    order_id,
                    dlc: actor.await,
                })
                .await
        });

        Ok(())
    }

    async fn handle_reject_order(&mut self, msg: RejectOrder) -> Result<()> {
        tracing::debug!(%msg.order_id, "Maker rejects an order" );

        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd_by_order_id(msg.order_id, &mut conn).await?;

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
            msg.order_id,
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
                command: TakerCommand::NotifyOrderRejected { id: msg.order_id },
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

    async fn handle_monitoring_event(&mut self, event: monitor::Event) -> Result<()> {
        let order_id = event.order_id();

        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        let new_state = cfd.handle(CfdStateChangeEvent::Monitor(event))?;

        insert_new_cfd_state_by_order_id(order_id, new_state.clone(), &mut conn).await?;

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
}

#[async_trait]
impl Handler<AcceptOrder> for Actor {
    async fn handle(&mut self, msg: AcceptOrder, ctx: &mut Context<Self>) {
        log_error!(self.handle_accept_order(msg, ctx))
    }
}

#[async_trait]
impl Handler<RejectOrder> for Actor {
    async fn handle(&mut self, msg: RejectOrder, _ctx: &mut Context<Self>) {
        log_error!(self.handle_reject_order(msg))
    }
}

#[async_trait]
impl Handler<NewOrder> for Actor {
    async fn handle(&mut self, msg: NewOrder, _ctx: &mut Context<Self>) {
        log_error!(self.handle_new_order(msg));
    }
}

#[async_trait]
impl Handler<NewTakerOnline> for Actor {
    async fn handle(&mut self, msg: NewTakerOnline, _ctx: &mut Context<Self>) {
        log_error!(self.handle_new_taker_online(msg));
    }
}

#[async_trait]
impl Handler<CfdSetupCompleted> for Actor {
    async fn handle(&mut self, msg: CfdSetupCompleted, _ctx: &mut Context<Self>) {
        log_error!(self.handle_cfd_setup_completed(msg));
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
        let TakerStreamMessage {
            taker_id: taker,
            item,
        } = msg;
        let msg = match item {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!(
                    "Error while receiving message from taker {}: {:#}",
                    taker,
                    e
                );
                return KeepRunning::Yes;
            }
        };

        match msg {
            wire::TakerToMaker::TakeOrder { order_id, quantity } => {
                log_error!(self.handle_take_order(taker, order_id, quantity))
            }
            wire::TakerToMaker::Protocol(msg) => {
                log_error!(self.handle_inc_protocol_msg(taker, msg))
            }
        }

        KeepRunning::Yes
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

impl Message for AcceptOrder {
    type Result = ();
}

impl Message for RejectOrder {
    type Result = ();
}

// this signature is a bit different because we use `Address::attach_stream`
impl Message for TakerStreamMessage {
    type Result = KeepRunning;
}

impl xtra::Actor for Actor {}
