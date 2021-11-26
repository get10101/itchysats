use std::time::Duration;

use crate::harness::flow::{is_next_none, next, next_cfd, next_custom_time, next_order, next_some};
use crate::harness::{
    dummy_new_order, init_tracing, order_from_cfd, start_both, Maker, MakerConfig, Taker,
    TakerConfig,
};
use daemon::connection::ConnectionStatus;
use daemon::maker_cfd::AcceptSettlement;
use daemon::model::cfd::CfdState;
use daemon::model::{Price, Usd};
use daemon::projection::Identity;
use daemon::{maker_cfd, monitor, taker_cfd};
use maia::secp256k1_zkp::schnorrsig;
use rust_decimal_macros::dec;
use tokio::time::sleep;
use xtra::prelude::MessageChannel;
mod harness;

#[tokio::test]
async fn taker_receives_order_from_maker_on_publication() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    assert!(is_next_none(taker.order_feed()).await.unwrap());

    maker.publish_order(dummy_new_order()).await;

    let (published, received) =
        tokio::join!(next_some(maker.order_feed()), next_some(taker.order_feed()));

    assert_eq!(published.unwrap(), received.unwrap());
}

#[tokio::test]
async fn taker_takes_order_and_maker_rejects() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    // TODO: Why is this needed? For the cfd stream it is not needed
    is_next_none(taker.order_feed()).await.unwrap();

    maker.publish_order(dummy_new_order()).await;

    let (_, received) = next_order(maker.order_feed(), taker.order_feed())
        .await
        .unwrap();

    taker.mocks.mock_oracle_announcement().await;
    taker.take_order(received.clone(), Usd::new(dec!(10))).await;

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    assert_eq!(order_from_cfd(&taker_cfd), received);
    assert_eq!(order_from_cfd(&maker_cfd), received);
    assert!(matches!(
        taker_cfd.state,
        CfdState::OutgoingOrderRequest { .. }
    ));
    assert!(matches!(
        maker_cfd.state,
        CfdState::IncomingOrderRequest { .. }
    ));

    maker.reject_take_request(received.clone()).await;

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    // TODO: More elaborate Cfd assertions
    assert_eq!(order_from_cfd(&taker_cfd), received);
    assert_eq!(order_from_cfd(&maker_cfd), received);
    assert!(matches!(taker_cfd.state, CfdState::Rejected { .. }));
    assert!(matches!(maker_cfd.state, CfdState::Rejected { .. }));
}

#[tokio::test]
#[ignore = "expensive, runs on CI"]
async fn taker_takes_order_and_maker_accepts_and_contract_setup() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    is_next_none(taker.order_feed()).await.unwrap();

    maker.publish_order(dummy_new_order()).await;

    let (_, received) = next_order(maker.order_feed(), taker.order_feed())
        .await
        .unwrap();

    taker.mocks.mock_oracle_announcement().await;
    taker.take_order(received.clone(), Usd::new(dec!(5))).await;
    let (_, _) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();

    maker.mocks.mock_oracle_announcement().await;
    taker.mocks.mock_oracle_announcement().await;

    maker.mocks.mock_party_params().await;
    taker.mocks.mock_party_params().await;

    maker.mocks.mock_monitor_oracle_attestation().await;
    taker.mocks.mock_monitor_oracle_attestation().await;

    maker.mocks.mock_oracle_monitor_attestation().await;
    taker.mocks.mock_oracle_monitor_attestation().await;

    maker.mocks.mock_monitor_start_monitoring().await;
    taker.mocks.mock_monitor_start_monitoring().await;

    maker.accept_take_request(received.clone()).await;

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::ContractSetup { .. }));
    assert!(matches!(maker_cfd.state, CfdState::ContractSetup { .. }));

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::PendingOpen { .. }));
    assert!(matches!(maker_cfd.state, CfdState::PendingOpen { .. }));
}

#[tokio::test]
async fn collab_close() {
    // somehow, changing this interval makes things *not* work :(
    let interval = Duration::from_secs(60);
    let _guard = init_tracing();
    let maker_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut maker = Maker::start(
        &MakerConfig::default().with_heartbeat_interval(interval),
        maker_listener,
    )
    .await;
    let mut taker = Taker::start(
        &TakerConfig::default().with_heartbeat_timeout(interval * 2),
        maker.listen_addr,
        maker.identity_pk,
    )
    .await;

    is_next_none(taker.order_feed()).await.unwrap();

    maker.publish_order(dummy_new_order()).await;

    let (_, received) = next_order(maker.order_feed(), taker.order_feed())
        .await
        .unwrap();

    taker.mocks.mock_oracle_announcement().await;
    taker.take_order(received.clone(), Usd::new(dec!(5))).await;
    let (_, _) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();

    maker.mocks.mock_oracle_announcement().await;
    taker.mocks.mock_oracle_announcement().await;

    maker.mocks.mock_party_params().await;
    taker.mocks.mock_party_params().await;

    maker.mocks.mock_monitor_oracle_attestation().await;
    taker.mocks.mock_monitor_oracle_attestation().await;

    maker.mocks.mock_oracle_monitor_attestation().await;
    taker.mocks.mock_oracle_monitor_attestation().await;

    maker.mocks.mock_monitor_start_monitoring().await;
    taker.mocks.mock_monitor_start_monitoring().await;

    maker.accept_take_request(received.clone()).await;

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::ContractSetup { .. }));
    assert!(matches!(maker_cfd.state, CfdState::ContractSetup { .. }));

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::PendingOpen { .. }));
    assert!(matches!(maker_cfd.state, CfdState::PendingOpen { .. }));

    // Make sure we're in 'open'
    maker
        .system
        .cfd_actor_addr
        .send(monitor::Event::LockFinality(maker_cfd.order.id))
        .await
        .unwrap();
    taker
        .system
        .cfd_actor_addr
        .send(monitor::Event::LockFinality(taker_cfd.order.id))
        .await
        .unwrap();

    dbg!("sent lock finality event to cfd actor");

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::Open { .. }));
    assert!(matches!(maker_cfd.state, CfdState::Open { .. }));

    ////////// WE'RE IN OPEN STATE, collab settlement starts here

    taker
        .system
        .cfd_actor_addr
        .send(taker_cfd::CfdAction::ProposeSettlement {
            order_id: taker_cfd.order.id,
            current_price: Price::new(dec!(10000)).unwrap(),
        })
        .await
        .unwrap()
        .unwrap();

    let wait_time = Duration::from_secs(5);

    // TODO: validate tighter
    let settlement_feed = next_custom_time(&mut maker.feeds.settlements, wait_time)
        .await
        .unwrap();

    assert!(settlement_feed.contains_key(&maker_cfd.order.id));

    maker
        .system
        .cfd_actor_addr
        .send(maker_cfd::AcceptSettlement {
            order_id: maker_cfd.order.id,
        })
        .await
        .unwrap()
        .unwrap();

    // TODO: validate tighter
    let settlement_feed = next_custom_time(&mut maker.feeds.settlements, wait_time)
        .await
        .unwrap();

    assert!(settlement_feed.is_empty());

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);

    // This assertions are replicating how to determine the UI-only state
    // "Pending Open"

    assert!(matches!(
        taker_cfd.state,
        CfdState::Open {
            collaborative_close: Some(_),
            ..
        }
    ));
    assert!(matches!(
        maker_cfd.state,
        CfdState::Open {
            collaborative_close: Some(_),
            ..
        }
    ));
}

#[tokio::test]
async fn taker_notices_lack_of_maker() {
    let short_interval = Duration::from_secs(1);

    let _guard = init_tracing();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

    let local_addr = listener.local_addr().unwrap();

    let maker_config = MakerConfig::default().with_heartbeat_interval(short_interval);

    let maker = Maker::start(&maker_config, listener).await;

    let taker_config = TakerConfig::default().with_heartbeat_timeout(short_interval * 2);

    let mut taker = Taker::start(&taker_config, maker.listen_addr, maker.identity_pk).await;

    assert_eq!(
        ConnectionStatus::Online,
        next(taker.maker_status_feed()).await.unwrap()
    );

    std::mem::drop(maker);

    sleep(taker_config.heartbeat_timeout).await;

    assert_eq!(
        ConnectionStatus::Offline,
        next(taker.maker_status_feed()).await.unwrap(),
    );

    let listener = tokio::net::TcpListener::bind(local_addr).await.unwrap();

    let _maker = Maker::start(&maker_config, listener).await;

    sleep(taker_config.heartbeat_timeout).await;

    assert_eq!(
        ConnectionStatus::Online,
        next(taker.maker_status_feed()).await.unwrap(),
    );
}

#[tokio::test]
async fn maker_notices_lack_of_taker() {
    let _guard = init_tracing();

    let (mut maker, taker) = start_both().await;
    assert_eq!(
        vec![taker.id.clone()],
        next(maker.connected_takers_feed()).await.unwrap()
    );

    std::mem::drop(taker);

    assert_eq!(
        Vec::<Identity>::new(),
        next(maker.connected_takers_feed()).await.unwrap()
    );
}
