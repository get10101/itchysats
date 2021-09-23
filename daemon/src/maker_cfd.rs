use crate::actors::log_error;
use crate::db::{
    insert_cfd, insert_new_cfd_state_by_order_id, insert_order, load_all_cfds,
    load_cfd_by_order_id, load_order_by_id,
};
use crate::maker_inc_connections::TakerCommand;
use crate::model::cfd::{Cfd, CfdState, CfdStateCommon, Dlc, Order, OrderId};
use crate::model::{TakerId, Usd};
use crate::wallet::Wallet;
use crate::wire::SetupMsg;
use crate::{maker_inc_connections, setup_contract_actor};
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::schnorrsig;
use std::time::SystemTime;
use tokio::sync::{mpsc, watch};
use xtra::prelude::*;

pub struct TakeOrder {
    pub taker_id: TakerId,
    pub order_id: OrderId,
    pub quantity: Usd,
}

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

pub struct IncProtocolMsg(pub SetupMsg);

pub struct CfdSetupCompleted {
    pub order_id: OrderId,
    pub dlc: Dlc,
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
}

impl Actor {
    pub async fn new(
        db: sqlx::SqlitePool,
        wallet: Wallet,
        oracle_pk: schnorrsig::PublicKey,
        cfd_feed_actor_inbox: watch::Sender<Vec<Cfd>>,
        order_feed_sender: watch::Sender<Option<Order>>,
        takers: Address<maker_inc_connections::Actor>,
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

    async fn handle_inc_protocol_msg(&mut self, msg: IncProtocolMsg) -> Result<()> {
        let msg = msg.0;
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

        let txid = self.wallet.try_broadcast_transaction(msg.dlc.lock).await?;

        tracing::info!("Lock transaction published with txid {}", txid);

        // TODO: tx monitoring, once confirmed with x blocks transition the Cfd to
        // Open
        Ok(())
    }

    async fn handle_take_order(&mut self, msg: TakeOrder) -> Result<()> {
        let TakeOrder {
            taker_id,
            order_id,
            quantity,
        } = msg;

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
            msg.quantity,
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

        self.takers
            .do_send_async(maker_inc_connections::TakerMessage {
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

        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;
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
}

#[async_trait]
impl Handler<TakeOrder> for Actor {
    async fn handle(&mut self, msg: TakeOrder, _ctx: &mut Context<Self>) {
        log_error!(self.handle_take_order(msg))
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
impl Handler<IncProtocolMsg> for Actor {
    async fn handle(&mut self, msg: IncProtocolMsg, _ctx: &mut Context<Self>) {
        log_error!(self.handle_inc_protocol_msg(msg));
    }
}

#[async_trait]
impl Handler<CfdSetupCompleted> for Actor {
    async fn handle(&mut self, msg: CfdSetupCompleted, _ctx: &mut Context<Self>) {
        log_error!(self.handle_cfd_setup_completed(msg));
    }
}

impl Message for TakeOrder {
    type Result = ();
}

impl Message for NewOrder {
    type Result = ();
}

impl Message for NewTakerOnline {
    type Result = ();
}

impl Message for IncProtocolMsg {
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

impl xtra::Actor for Actor {}
