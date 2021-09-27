use crate::actors::log_error;
use crate::model::cfd::{Order, OrderId};
use crate::model::TakerId;
use crate::wire::SetupMsg;
use crate::{maker_cfd, send_to_socket, wire};
use anyhow::{Context as AnyhowContext, Result};
use async_trait::async_trait;
use futures::{Future, StreamExt};
use std::collections::HashMap;
use tokio::net::tcp::OwnedReadHalf;
use tokio_util::codec::FramedRead;
use xtra::prelude::*;

pub struct BroadcastOrder(pub Option<Order>);

impl Message for BroadcastOrder {
    type Result = Result<()>;
}

#[allow(clippy::large_enum_variant)]
pub enum TakerCommand {
    SendOrder { order: Option<Order> },
    NotifyInvalidOrderId { id: OrderId },
    NotifyOrderAccepted { id: OrderId },
    NotifyOrderRejected { id: OrderId },
    OutProtocolMsg { setup_msg: SetupMsg },
}

pub struct TakerMessage {
    pub taker_id: TakerId,
    pub command: TakerCommand,
}

impl Message for TakerMessage {
    type Result = Result<()>;
}

pub struct NewTakerOnline {
    pub taker_id: TakerId,
    pub out_msg_actor: Address<send_to_socket::Actor<wire::MakerToTaker>>,
}

impl Message for NewTakerOnline {
    type Result = Result<()>;
}

pub struct Actor {
    write_connections: HashMap<TakerId, Address<send_to_socket::Actor<wire::MakerToTaker>>>,
    cfd_actor: Address<maker_cfd::Actor>,
}

impl xtra::Actor for Actor {}

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
        conn.do_send_async(msg).await?;

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
            TakerCommand::OutProtocolMsg { setup_msg } => {
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
    async fn handle(&mut self, msg: BroadcastOrder, _ctx: &mut Context<Self>) -> Result<()> {
        log_error!(self.handle_broadcast_order(msg));
        Ok(())
    }
}

#[async_trait]
impl Handler<TakerMessage> for Actor {
    async fn handle(&mut self, msg: TakerMessage, _ctx: &mut Context<Self>) -> Result<()> {
        log_error!(self.handle_taker_message(msg));
        Ok(())
    }
}

#[async_trait]
impl Handler<NewTakerOnline> for Actor {
    async fn handle(&mut self, msg: NewTakerOnline, _ctx: &mut Context<Self>) -> Result<()> {
        log_error!(self.handle_new_taker_online(msg));
        Ok(())
    }
}

//

pub fn in_taker_messages(
    read: OwnedReadHalf,
    cfd_actor_inbox: Address<maker_cfd::Actor>,
    taker_id: TakerId,
) -> impl Future<Output = ()> {
    let mut messages = FramedRead::new(read, wire::JsonCodec::new());

    async move {
        while let Some(message) = messages.next().await {
            match message {
                Ok(wire::TakerToMaker::TakeOrder { order_id, quantity }) => {
                    cfd_actor_inbox
                        .do_send_async(maker_cfd::TakeOrder {
                            taker_id,
                            order_id,
                            quantity,
                        })
                        .await
                        .unwrap();
                }
                Ok(wire::TakerToMaker::Protocol(msg)) => {
                    cfd_actor_inbox
                        .do_send_async(maker_cfd::IncProtocolMsg(msg))
                        .await
                        .unwrap();
                }
                Err(error) => {
                    tracing::error!(%error, "Error in reading message");
                }
            }
        }
    }
}
