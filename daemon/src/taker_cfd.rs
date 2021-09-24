use crate::db::{
    insert_cfd, insert_new_cfd_state_by_order_id, insert_order, load_all_cfds,
    load_cfd_by_order_id, load_order_by_id,
};

use crate::actors::log_error;
use crate::model::cfd::{Cfd, CfdState, CfdStateCommon, Dlc, Order, OrderId};
use crate::model::Usd;
use crate::wallet::Wallet;
use crate::wire::SetupMsg;
use crate::{setup_contract_actor, wire};
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::schnorrsig;
use std::time::SystemTime;
use tokio::sync::{mpsc, watch};
use xtra::prelude::*;

pub struct TakeOffer {
    pub order_id: OrderId,
    pub quantity: Usd,
}

pub struct NewOrder(pub Option<Order>);

pub struct OrderAccepted(pub OrderId);
pub struct OrderRejected(pub OrderId);
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
    order_feed_actor_inbox: watch::Sender<Option<Order>>,
    out_msg_maker_inbox: mpsc::UnboundedSender<wire::TakerToMaker>,
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
        order_feed_actor_inbox: watch::Sender<Option<Order>>,
        out_msg_maker_inbox: mpsc::UnboundedSender<wire::TakerToMaker>,
    ) -> Result<Self> {
        let mut conn = db.acquire().await?;
        cfd_feed_actor_inbox.send(load_all_cfds(&mut conn).await?)?;

        Ok(Self {
            db,
            wallet,
            oracle_pk,
            cfd_feed_actor_inbox,
            order_feed_actor_inbox,
            out_msg_maker_inbox,
            current_contract_setup: None,
            contract_setup_message_buffer: vec![],
        })
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
        self.out_msg_maker_inbox
            .send(wire::TakerToMaker::TakeOrder { order_id, quantity })?;

        Ok(())
    }

    async fn handle_new_order(&mut self, order: Option<Order>) -> Result<()> {
        match order {
            Some(order) => {
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

        let (sk, pk) = crate::keypair::new(&mut rand::thread_rng());

        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;
        let margin = cfd.margin()?;

        let taker_params = self.wallet.build_party_params(margin, pk).await?;

        let (actor, inbox) = setup_contract_actor::new(
            {
                let inbox = self.out_msg_maker_inbox.clone();
                move |msg| inbox.send(wire::TakerToMaker::Protocol(msg)).unwrap()
            },
            setup_contract_actor::OwnParams::Taker(taker_params),
            sk,
            self.oracle_pk,
            cfd,
            self.wallet.clone(),
        );

        for msg in self.contract_setup_message_buffer.drain(..) {
            inbox.send(msg)?;
        }

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

        self.current_contract_setup = Some(inbox);
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

    async fn handle_cfd_setup_completed(&mut self, order_id: OrderId, dlc: Dlc) -> Result<()> {
        tracing::info!("Setup complete, publishing on chain now");

        self.current_contract_setup = None;
        self.contract_setup_message_buffer = vec![];

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

        let txid = self.wallet.try_broadcast_transaction(dlc.lock).await?;

        tracing::info!("Lock transaction published with txid {}", txid);

        // TODO: tx monitoring, once confirmed with x blocks transition the Cfd to
        // Open

        Ok(())
    }
}

#[async_trait]
impl Handler<TakeOffer> for Actor {
    async fn handle(&mut self, msg: TakeOffer, _ctx: &mut Context<Self>) {
        log_error!(self.handle_take_offer(msg.order_id, msg.quantity));
    }
}

#[async_trait]
impl Handler<NewOrder> for Actor {
    async fn handle(&mut self, msg: NewOrder, _ctx: &mut Context<Self>) {
        log_error!(self.handle_new_order(msg.0));
    }
}

#[async_trait]
impl Handler<OrderAccepted> for Actor {
    async fn handle(&mut self, msg: OrderAccepted, ctx: &mut Context<Self>) {
        log_error!(self.handle_order_accepted(msg.0, ctx));
    }
}

#[async_trait]
impl Handler<OrderRejected> for Actor {
    async fn handle(&mut self, msg: OrderRejected, _ctx: &mut Context<Self>) {
        log_error!(self.handle_order_rejected(msg.0));
    }
}

#[async_trait]
impl Handler<IncProtocolMsg> for Actor {
    async fn handle(&mut self, msg: IncProtocolMsg, _ctx: &mut Context<Self>) {
        log_error!(self.handle_inc_protocol_msg(msg.0));
    }
}

#[async_trait]
impl Handler<CfdSetupCompleted> for Actor {
    async fn handle(&mut self, msg: CfdSetupCompleted, _ctx: &mut Context<Self>) {
        log_error!(self.handle_cfd_setup_completed(msg.order_id, msg.dlc));
    }
}

impl Message for TakeOffer {
    type Result = ();
}

impl Message for NewOrder {
    type Result = ();
}

impl Message for OrderAccepted {
    type Result = ();
}

impl Message for OrderRejected {
    type Result = ();
}

impl Message for IncProtocolMsg {
    type Result = ();
}

impl Message for CfdSetupCompleted {
    type Result = ();
}

impl xtra::Actor for Actor {}
