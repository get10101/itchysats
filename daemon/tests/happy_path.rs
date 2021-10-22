use daemon::maker_cfd;
use daemon::model::cfd::Order;
use tokio::sync::watch;

#[tokio::test]
async fn taker_receives_order_from_maker_on_publication() {
    let (maker, taker) = start_both().await;

    let (published, received) = tokio::join!(
        maker.publish_order(maker_cfd::NewOrder::dummy()),
        taker.next_order()
    );

    assert_eq!(published, received)
}

/// Test Stub simulating the Oracle actor
struct Oracle;

/// Test Stub simulating the Monitor actor
struct Monitor;

/// Test Stub simulating the TakerConnections actor
struct TakerConnections;

/// Maker Test Setup
struct Maker {
    cfd_actor: xtra::Address<maker_cfd::Actor<Oracle, Monitor, TakerConnections>>,
    order_feed: watch::Receiver<Option<Order>>,
}

impl Maker {
    async fn start() -> Self {
        Self {
            cfd_actor: todo!("plug in cfd actor"),
            order_feed: todo!("plug in order feed"),
        }
    }

    async fn publish_order(&mut self, new_order_params: maker_cfd::NewOrder) -> Order {
        self.cfd_actor.send(new_order_params).await.unwrap();
        let next_order = self.order_feed.borrow().clone().unwrap();

        next_order
    }
}

/// Taker Test Setup
struct Taker {
    order_feed: watch::Receiver<Option<Order>>,
}

impl Taker {
    async fn start() -> Self {
        Self {
            order_feed: todo!("plug in order feed "),
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
    let taker = Taker::start().await;

    (maker, taker)
}
