use crate::actors::log_error;
use crate::model::cfd::{Order, OrderId};
use crate::model::TakerId;
use crate::{maker_cfd, send_to_socket, wire};
use anyhow::{Context as AnyhowContext, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use xtra::prelude::*;

pub struct BroadcastOrder(pub Option<Order>);

#[allow(clippy::large_enum_variant)]
pub enum TakerCommand {
    SendOrder { order: Option<Order> },
    NotifyInvalidOrderId { id: OrderId },
    NotifyOrderAccepted { id: OrderId },
    NotifyOrderRejected { id: OrderId },
    Protocol(wire::SetupMsg),
}

pub struct TakerMessage {
    pub taker_id: TakerId,
    pub command: TakerCommand,
}

pub struct NewTakerOnline {
    pub taker_id: TakerId,
    pub out_msg_actor: Address<send_to_socket::Actor<wire::MakerToTaker>>,
}

pub struct Actor {
    write_connections: HashMap<TakerId, Address<send_to_socket::Actor<wire::MakerToTaker>>>,
    cfd_actor: Address<maker_cfd::Actor>,
}

impl Actor {
    pub fn new(cfd_actor: Address<maker_cfd::Actor>) -> Self {
        Self {
            write_connections: HashMap::new(),
            cfd_actor,
        }
    }

    async fn send_to_taker(&self, taker_id: TakerId, msg: wire::MakerToTaker) -> Result<()> {
        let conn = self
            .write_connections
            .get(&taker_id)
            .context("no connection to taker_id")?;

        // use `.send` here to ensure we only continue once the message has been sent
        conn.send(msg).await?;

        Ok(())
    }

    async fn handle_broadcast_order(&mut self, msg: BroadcastOrder) -> Result<()> {
        let order = msg.0;

        for conn in self.write_connections.values() {
            conn.do_send_async(wire::MakerToTaker::CurrentOrder(order.clone()))
                .await?;
        }

        Ok(())
    }

    async fn handle_taker_message(&mut self, msg: TakerMessage) -> Result<()> {
        match msg.command {
            TakerCommand::SendOrder { order } => {
                self.send_to_taker(msg.taker_id, wire::MakerToTaker::CurrentOrder(order))
                    .await?;
            }
            TakerCommand::NotifyInvalidOrderId { id } => {
                self.send_to_taker(msg.taker_id, wire::MakerToTaker::InvalidOrderId(id))
                    .await?;
            }
            TakerCommand::NotifyOrderAccepted { id } => {
                self.send_to_taker(msg.taker_id, wire::MakerToTaker::ConfirmOrder(id))
                    .await?;
            }
            TakerCommand::NotifyOrderRejected { id } => {
                self.send_to_taker(msg.taker_id, wire::MakerToTaker::RejectOrder(id))
                    .await?;
            }
            TakerCommand::Protocol(setup_msg) => {
                self.send_to_taker(msg.taker_id, wire::MakerToTaker::Protocol(setup_msg))
                    .await?;
            }
        }
        Ok(())
    }

    async fn handle_new_taker_online(&mut self, msg: NewTakerOnline) -> Result<()> {
        self.cfd_actor
            .do_send_async(maker_cfd::NewTakerOnline { id: msg.taker_id })
            .await?;

        self.write_connections
            .insert(msg.taker_id, msg.out_msg_actor);
        Ok(())
    }
}

macro_rules! log_error {
    ($future:expr) => {
        if let Err(e) = $future.await {
            tracing::error!(%e);
        }
    };
}

#[async_trait]
impl Handler<BroadcastOrder> for Actor {
    async fn handle(&mut self, msg: BroadcastOrder, _ctx: &mut Context<Self>) {
        log_error!(self.handle_broadcast_order(msg));
    }
}

#[async_trait]
impl Handler<TakerMessage> for Actor {
    async fn handle(&mut self, msg: TakerMessage, _ctx: &mut Context<Self>) {
        log_error!(self.handle_taker_message(msg));
    }
}

#[async_trait]
impl Handler<NewTakerOnline> for Actor {
    async fn handle(&mut self, msg: NewTakerOnline, _ctx: &mut Context<Self>) {
        log_error!(self.handle_new_taker_online(msg));
    }
}

impl Message for BroadcastOrder {
    type Result = ();
}

impl Message for TakerMessage {
    type Result = ();
}

impl Message for NewTakerOnline {
    type Result = ();
}

impl xtra::Actor for Actor {}
