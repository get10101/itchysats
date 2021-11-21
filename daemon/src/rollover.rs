use anyhow::Result;
use crate::{connection, wire};
use crate::model::cfd::{Dlc, OrderId};
use crate::model::{BitMexPriceEventId, Timestamp};
use crate::wire::RollOverMsg;
use async_trait::async_trait;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;

/// Handles the entire lifecyle of a rollover.
///
/// The actors lifecycle is tied to the lifecycle of the rollover. Creating a new instance of this
/// actor starts the rollover. The actor will shut itself down once the rollover is finished, either
/// because it was accepted and completed or rejected.
pub struct Actor {
    /// The id of the order we are meant to roll over.
    order_id: OrderId,
    dlc: Dlc,
    connection: xtra::Address<connection::Actor>,
    on_completed: Box<dyn MessageChannel<Completed>>
}

impl Actor {
    /// Create a new instance of the roll over actor.
    pub fn new(order_id: OrderId) -> Self {


        Self {
            order_id,
            connection: todo!(),
        }
    }
}

pub struct Rejected;
pub struct Confirmed {
    oracle_event_id: BitMexPriceEventId,
}

pub enum Completed {
    NewContract {
        order_id: OrderId,
        dlc: Dlc,
    },
    Rejected {
        order_id: OrderId,
    },
    Failed {
        order_id: OrderId,
    	error: anyhow::Error
    }
}

#[xtra_productivity]
impl Actor {
    fn handle(&mut self, msg: Rejected, ctx: &mut xtra::Context<Self>) {
    	self.on_completed.send(Completed::Rejected { order_id: self.order_id }).await;
    	ctx.stop();
    }

    fn handle(&mut self, msg: Confirmed, ctx: &mut xtra::Context<Self>) {
    	// tracing::info!(%order_id, "Roll; over request got accepted");

     //    let (sender, receiver) = mpsc::unbounded();

     //    let announcement = self
     //        .oracle_actor
     //        .send(oracle::GetAnnouncement(oracle_event_id))
     //        .await?
     //        .with_context(|| format!("Announcement {} not found", oracle_event_id))?;

     //    let contract_future = setup_contract::roll_over(
     //        self.send_to_maker.sink().with(move |msg| {
     //            future::ok(wire::TakerToMaker::RollOverProtocol { order_id, msg })
     //        }),
     //        receiver,
     //        (self.oracle_pk, announcement),
     //        cfd,
     //        Role::Taker,
     //        self.dlc,
     //        self.n_payouts,
     //    );

     //    let this = ctx
     //        .address()
     //        .expect("actor to be able to give address to itself");

     //    let task = async move {
     //        let dlc = contract_future.await;

     //        this.send(CfdRollOverCompleted { order_id, dlc })
     //            .await
     //            .expect("always connected to ourselves")
     //    }
     //    .spawn_with_handle();

     //    self.roll_over_state = RollOverState::Active {
     //        sender,
     //        _task: task,
     //    };

     //    self.remove_pending_proposal(&order_id)
     //        .context("Could not remove accepted roll over")?;
     //    Ok(())
    }

    fn handle(&mut self, msg: RollOverMsg, ctx: &mut xtra::Context<Self>) {
    	todo!("get channel from roll-over state to forward message to task")
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        // 1. propose roll over to maker

        // let proposal = RollOverProposal {
        //     order_id,
        //     timestamp: Timestamp::now()?,
        // };

        // self.current_pending_proposals.insert(
        //     proposal.order_id,
        //     UpdateCfdProposal::RollOverProposal {
        //         proposal: proposal.clone(),
        //         direction: SettlementKind::Outgoing,
        //     },
        // );
        // self.send_pending_update_proposals()?;

        self.connection
            .send(wire::TakerToMaker::ProposeRollOver {
                order_id: self.order_id,
                timestamp: Timestamp::now().unwrap(),
            })
            .await?;
    }
}
