use async_trait::async_trait;
use daemon::model::cfd::Order;
use daemon::{connection, maker_cfd, maker_inc_connections, monitor, oracle};
use std::net::SocketAddr;
use std::task::Poll;
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

/// Maker Test Setup
struct Maker {
    cfd_actor_addr: xtra::Address<maker_cfd::Actor<Oracle, Monitor, maker_inc_connections::Actor>>,
    order_feed_receiver: watch::Receiver<Option<Order>>,
    inc_conn_addr: xtra::Address<maker_inc_connections::Actor>,
    address: SocketAddr,
}

impl Maker {
    async fn start() -> Self {
        let maker = daemon::Maker::new(
            todo!("db"),
            todo!("wallet"),
            todo!("oracle_pk"),
            |_, _| Oracle,
            |_, _| async { Ok(Monitor) },
            |channel0, channel1| maker_inc_connections::Actor::new(channel0, channel1),
        )
        .await
        .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

        let address = listener.local_addr().unwrap();

        let listener_stream = futures::stream::poll_fn(move |ctx| {
            let message = match futures::ready!(listener.poll_accept(ctx)) {
                Ok((stream, address)) => {
                    maker_inc_connections::ListenerMessage::NewConnection { stream, address }
                }
                Err(e) => maker_inc_connections::ListenerMessage::Error { source: e },
            };

            Poll::Ready(Some(message))
        });

        tokio::spawn(maker.inc_conn_addr.attach_stream(listener_stream));

        Self {
            cfd_actor_addr: maker.cfd_actor_addr,
            order_feed_receiver: maker.order_feed_receiver,
            inc_conn_addr: maker.inc_conn_addr,
            address,
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
    async fn start(maker_address: SocketAddr) -> Self {
        let connection::Actor {
            send_to_maker,
            read_from_maker,
        } = connection::Actor::new(maker_address).await;

        let taker = daemon::Taker::new(
            todo!("db"),
            todo!("wallet"),
            todo!("oracle_pk"),
            send_to_maker,
            read_from_maker,
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
    let maker = Maker::start().await;
    let taker = Taker::start(maker.address).await;
    (maker, taker)
}
