use crate::maker_cfd::{NewTakerOnline, TakerStreamMessage};
use crate::model::cfd::{Order, OrderId};
use crate::model::{BitMexPriceEventId, TakerId};
use crate::{log_error, maker_cfd, send_to_socket, wire};
use anyhow::{Context as AnyhowContext, Result};
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio_util::codec::FramedRead;
use xtra::prelude::*;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::{Actor as _, KeepRunning};

pub struct BroadcastOrder(pub Option<Order>);

#[allow(clippy::large_enum_variant)]
pub enum TakerCommand {
    SendOrder {
        order: Option<Order>,
    },
    NotifyInvalidOrderId {
        id: OrderId,
    },
    NotifyOrderAccepted {
        id: OrderId,
    },
    NotifyOrderRejected {
        id: OrderId,
    },
    NotifySettlementAccepted {
        id: OrderId,
    },
    NotifySettlementRejected {
        id: OrderId,
    },
    NotifyRollOverAccepted {
        id: OrderId,
        oracle_event_id: BitMexPriceEventId,
    },
    NotifyRollOverRejected {
        id: OrderId,
    },
    Protocol(wire::SetupMsg),
    RollOverProtocol(wire::RollOverMsg),
}

pub struct TakerMessage {
    pub taker_id: TakerId,
    pub command: TakerCommand,
}

pub enum ListenerMessage {
    NewConnection {
        stream: TcpStream,
        address: SocketAddr,
    },
    Error {
        source: io::Error,
    },
}

pub struct Actor {
    write_connections: HashMap<TakerId, Address<send_to_socket::Actor<wire::MakerToTaker>>>,
    new_taker_channel: Box<dyn MessageChannel<NewTakerOnline>>,
    taker_msg_channel: Box<dyn MessageChannel<TakerStreamMessage>>,
}

impl Actor {
    pub fn new(
        new_taker_channel: &impl MessageChannel<NewTakerOnline>,
        taker_msg_channel: &impl MessageChannel<TakerStreamMessage>,
    ) -> Self {
        Self {
            write_connections: HashMap::new(),
            new_taker_channel: new_taker_channel.clone_channel(),
            taker_msg_channel: taker_msg_channel.clone_channel(),
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
            TakerCommand::NotifySettlementAccepted { id } => {
                self.send_to_taker(msg.taker_id, wire::MakerToTaker::ConfirmSettlement(id))
                    .await?;
            }
            TakerCommand::NotifySettlementRejected { id } => {
                self.send_to_taker(msg.taker_id, wire::MakerToTaker::RejectSettlement(id))
                    .await?;
            }
            TakerCommand::Protocol(setup_msg) => {
                self.send_to_taker(msg.taker_id, wire::MakerToTaker::Protocol(setup_msg))
                    .await?;
            }
            TakerCommand::NotifyRollOverAccepted {
                id,
                oracle_event_id,
            } => {
                self.send_to_taker(
                    msg.taker_id,
                    wire::MakerToTaker::ConfirmRollOver {
                        order_id: id,
                        oracle_event_id,
                    },
                )
                .await?;
            }
            TakerCommand::NotifyRollOverRejected { id } => {
                self.send_to_taker(msg.taker_id, wire::MakerToTaker::RejectRollOver(id))
                    .await?;
            }
            TakerCommand::RollOverProtocol(roll_over_msg) => {
                self.send_to_taker(
                    msg.taker_id,
                    wire::MakerToTaker::RollOverProtocol(roll_over_msg),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn handle_new_connection(
        &mut self,
        stream: TcpStream,
        address: SocketAddr,
        ctx: &mut Context<Self>,
    ) {
        let taker_id = TakerId::default();

        tracing::info!("New taker {} connected on {}", taker_id, address);

        let (read, write) = stream.into_split();
        let read = FramedRead::new(read, wire::JsonCodec::default())
            .map(move |item| maker_cfd::TakerStreamMessage { taker_id, item });

        let this = ctx.address().expect("self to be alive");
        tokio::spawn(this.attach_stream(Box::pin(read)));

        let out_msg_actor = send_to_socket::Actor::new(write)
            .create(None)
            .spawn_global();

        self.write_connections.insert(taker_id, out_msg_actor);

        let _ = self
            .new_taker_channel
            .send(maker_cfd::NewTakerOnline { id: taker_id })
            .await;
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
impl Handler<ListenerMessage> for Actor {
    async fn handle(&mut self, msg: ListenerMessage, ctx: &mut Context<Self>) -> KeepRunning {
        match msg {
            ListenerMessage::NewConnection { stream, address } => {
                self.handle_new_connection(stream, address, ctx).await;

                KeepRunning::Yes
            }
            ListenerMessage::Error { source } => {
                tracing::warn!("TCP listener produced an error: {}", source);

                // Maybe we should move the actual listening on the socket into here and restart the
                // actor upon an error?
                KeepRunning::Yes
            }
        }
    }
}

#[async_trait]
impl Handler<TakerStreamMessage> for Actor {
    async fn handle(&mut self, msg: TakerStreamMessage, _ctx: &mut Context<Self>) -> KeepRunning {
        log_error!(self.taker_msg_channel.send(msg));
        KeepRunning::Yes
    }
}

impl Message for BroadcastOrder {
    type Result = ();
}

impl Message for TakerMessage {
    type Result = ();
}

impl Message for ListenerMessage {
    type Result = KeepRunning;
}

impl xtra::Actor for Actor {}
