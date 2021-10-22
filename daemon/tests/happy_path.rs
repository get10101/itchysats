use anyhow::Result;
use async_trait::async_trait;
use cfd_protocol::Announcement;
use daemon::maker_cfd::NewTakerOnline;
use daemon::maker_inc_connections::TakerCommand;
use daemon::model::cfd::Order;
use daemon::{maker_cfd, maker_inc_connections, monitor, oracle, taker_cfd, wire};
use futures::channel::mpsc::{self, UnboundedSender};
use tokio::sync::watch;
use xtra::message_channel::MessageChannel;
use xtra_productivity::xtra_productivity;

#[tokio::test]
async fn taker_receives_order_from_maker_on_publication() {
    let (mut maker, mut taker) = start_both().await;

    let (published, received) =
        tokio::join!(maker.publish_order(new_dummy_order()), taker.next_order());

    assert_eq!(published, received)
}

fn new_dummy_order() -> maker_cfd::NewOrder {
    todo!("dummy new order")
}

// Mocks the network layer between the taker and the maker ("the wire")
struct ActorConnection {}
impl xtra::Actor for ActorConnection {}

#[xtra_productivity(message_impl = false)]
impl ActorConnection {}

/// Test Stub simulating the Oracle actor
struct Oracle;
impl xtra::Actor for Oracle {}

#[xtra_productivity(message_impl = false)]
impl Oracle {
    async fn handle_fetch_announcement(&mut self, _msg: oracle::FetchAnnouncement) {
        todo!("stub this if needed")
    }

    async fn handle_get_announcement(
        &mut self,
        _msg: oracle::GetAnnouncement,
    ) -> Option<oracle::Announcement> {
        todo!("stub this if needed")
    }

    async fn handle_monitor_attestation(&mut self, _msg: oracle::MonitorAttestation) {
        todo!("stub this if needed")
    }

    async fn handle_sync(&mut self, _msg: oracle::Sync) {
        todo!("stub this if needed")
    }
}

/// Test Stub simulating the Monitor actor
struct Monitor;
impl xtra::Actor for Monitor {}

#[xtra_productivity(message_impl = false)]
impl Monitor {
    async fn handle_sync(&mut self, _msg: monitor::Sync) {
        todo!("stub this if needed")
    }

    async fn handle_start_monitoring(&mut self, _msg: monitor::StartMonitoring) {
        todo!("stub this if needed")
    }

    async fn handle_collaborative_settlement(&mut self, _msg: monitor::CollaborativeSettlement) {
        todo!("stub this if needed")
    }

    async fn handle_oracle_attestation(&mut self, _msg: oracle::Attestation) {
        todo!("stub this if needed")
    }
}

/// Test Stub simulating the MakerIncConnections actor
struct MakerIncConnections {
    new_taker_online: Box<dyn MessageChannel<NewTakerOnline>>,
    send_to_taker: UnboundedSender<taker_cfd::MakerStreamMessage>,
}

impl xtra::Actor for MakerIncConnections {}

#[xtra_productivity(message_impl = false)]
impl MakerIncConnections {
    async fn broadcast_order(&mut self, _msg: maker_inc_connections::BroadcastOrder) -> Result<()> {
        todo!("forward order to taker")
    }

    async fn taker_message(&mut self, msg: maker_inc_connections::TakerMessage) -> Result<()> {
        todo!("we're using send_to_taker() here");

        match msg.command {
            TakerCommand::SendOrder { order } => {
                self.send_to_taker(msg.taker_id, wire::MakerToTaker::CurrentOrder(order))
                    .await?;
            }
            _ => panic!("boom"),
        }
    }

    async fn taker_wire_message(&mut self, _msg: wire::TakerToMaker) -> () {
        todo!("we're using send_to_taker() here");
    }
}

/// Maker Test Setup
struct Maker {
    cfd_actor_addr: xtra::Address<maker_cfd::Actor<Oracle, Monitor, MakerIncConnections>>,
    order_feed_receiver: watch::Receiver<Option<Order>>,
    inc_conn_addr: xtra::Address<MakerIncConnections>,
}

impl Maker {
    async fn start(send_to_taker: UnboundedSender<taker_cfd::MakerStreamMessage>) -> Self {
        let maker = daemon::Maker::new(
            todo!("db"),
            todo!("wallet"),
            todo!("oracle_pk"),
            |_, _| Oracle,
            |_, _| async { Ok(Monitor) },
            move |new_taker_online, _| MakerIncConnections {
                new_taker_online,
                send_to_taker,
            },
        )
        .await
        .unwrap();

        Self {
            cfd_actor_addr: maker.cfd_actor_addr,
            order_feed_receiver: maker.order_feed_receiver,
            inc_conn_addr: maker.inc_conn_addr,
        }
    }

    async fn publish_order(&mut self, new_order_params: maker_cfd::NewOrder) -> Order {
        self.cfd_actor_addr.send(new_order_params).await.unwrap();
        let next_order = self.order_feed_receiver.borrow().clone().unwrap();

        next_order
    }
}

/// Taker Test Setup
struct Taker {
    order_feed: watch::Receiver<Option<Order>>,
}

impl Taker {
    async fn start(
        maker_inc_connections: xtra::Address<MakerIncConnections>,
        read_from_maker: mpsc::UnboundedReceiver<taker_cfd::MakerStreamMessage>,
    ) -> Self {
        let taker = daemon::Taker::new(
            todo!("db"),
            todo!("wallet"),
            todo!("oracle_pk"),
            Box::new(maker_inc_connections),
            Box::new(read_from_maker),
            |_, _| Oracle,
            |_, _| async { Ok(Monitor) },
        )
        .await
        .unwrap();

        Self {
            order_feed: taker.order_feed_receiver,
        }
    }

    async fn next_order(&mut self) -> Order {
        self.order_feed.changed().await.unwrap();
        let next_order = self.order_feed.borrow().clone().unwrap();
        next_order
    }
}

async fn start_both() -> (Maker, Taker) {
    let (sender, recv) = mpsc::unbounded();

    let maker = Maker::start(sender).await;

    let taker = Taker::start(maker.inc_conn_addr.clone(), recv).await;

    (maker, taker)
}
