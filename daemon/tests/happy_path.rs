use std::time::Duration;

use crate::harness::dummy_new_order;
use crate::harness::flow::is_next_none;
use crate::harness::flow::next;
use crate::harness::flow::next_cfd;
use crate::harness::flow::next_order;
use crate::harness::flow::next_some;
use crate::harness::init_tracing;
use crate::harness::start_both;
use crate::harness::Maker;
use crate::harness::MakerConfig;
use crate::harness::Taker;
use crate::harness::TakerConfig;
use daemon::connection::ConnectionStatus;
use daemon::model::cfd::OrderId;
use daemon::model::Usd;
use daemon::monitor::Event;
use daemon::projection::CfdState;
use daemon::projection::Identity;
use maia::secp256k1_zkp::schnorrsig;
use rust_decimal_macros::dec;
use tokio::time::sleep;
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
    assert_eq!(taker_cfd.order_id, received.id);
    assert_eq!(maker_cfd.order_id, received.id);
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
    assert_eq!(taker_cfd.order_id, received.id);
    assert_eq!(maker_cfd.order_id, received.id);
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
    maker.mocks.mock_oracle_announcement().await;

    taker.take_order(received.clone(), Usd::new(dec!(5))).await;
    let (_, _) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();

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
    assert_eq!(taker_cfd.order_id, received.id);
    assert_eq!(maker_cfd.order_id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::ContractSetup { .. }));
    assert!(matches!(maker_cfd.state, CfdState::ContractSetup { .. }));

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order_id, received.id);
    assert_eq!(maker_cfd.order_id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::PendingOpen { .. }));
    assert!(matches!(maker_cfd.state, CfdState::PendingOpen { .. }));

    deliver_event!(maker, taker, Event::LockFinality(received.id));

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    assert!(matches!(taker_cfd.state, CfdState::Open { .. }));
    assert!(matches!(maker_cfd.state, CfdState::Open { .. }));
}

#[tokio::test]
async fn collaboratively_close_an_open_cfd() {
    let _guard = init_tracing();
    let (mut maker, mut taker, order_id) = start_from_open_cfd_state().await;

    taker.propose_settlement(order_id).await;

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    assert!(matches!(
        taker_cfd.state,
        CfdState::OutgoingSettlementProposal { .. }
    ));
    assert!(matches!(
        maker_cfd.state,
        CfdState::IncomingSettlementProposal { .. }
    ));

    maker.mocks.mock_monitor_collaborative_settlement().await;
    taker.mocks.mock_monitor_collaborative_settlement().await;

    maker.accept_settlement_proposal(order_id).await;
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    assert!(matches!(taker_cfd.state, CfdState::PendingClose { .. }));
    assert!(matches!(maker_cfd.state, CfdState::PendingClose { .. }));

    deliver_event!(maker, taker, Event::CloseFinality(order_id));

    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    assert!(matches!(taker_cfd.state, CfdState::Closed { .. }));
    assert!(matches!(maker_cfd.state, CfdState::Closed { .. }));
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

    let mut taker = Taker::start(&taker_config, maker.listen_addr, maker.identity).await;

    assert_eq!(
        ConnectionStatus::Online,
        next(taker.maker_status_feed()).await.unwrap()
    );

    std::mem::drop(maker);

    sleep(taker_config.heartbeat_timeout).await;

    assert_eq!(
        ConnectionStatus::Offline { reason: None },
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

/// Hide the implementation detail of arriving at the Cfd open state.
/// Useful when reading tests that should start at this point.
/// For convenience, returns also OrderId of the opened Cfd.
async fn start_from_open_cfd_state() -> (Maker, Taker, OrderId) {
    let heartbeat_interval = Duration::from_secs(60);
    let maker_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut maker = Maker::start(
        &MakerConfig::default().with_heartbeat_interval(heartbeat_interval),
        maker_listener,
    )
    .await;
    let mut taker = Taker::start(
        &TakerConfig::default().with_heartbeat_timeout(heartbeat_interval * 2),
        maker.listen_addr,
        maker.identity,
    )
    .await;

    is_next_none(taker.order_feed()).await.unwrap();

    maker.publish_order(dummy_new_order()).await;

    let (_, received) = next_order(maker.order_feed(), taker.order_feed())
        .await
        .unwrap();

    taker.mocks.mock_oracle_announcement().await;
    maker.mocks.mock_oracle_announcement().await;

    taker.take_order(received.clone(), Usd::new(dec!(5))).await;
    let (_, _) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();

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
    assert!(matches!(taker_cfd.state, CfdState::ContractSetup { .. }));
    assert!(matches!(maker_cfd.state, CfdState::ContractSetup { .. }));

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    assert!(matches!(taker_cfd.state, CfdState::PendingOpen { .. }));
    assert!(matches!(maker_cfd.state, CfdState::PendingOpen { .. }));

    deliver_event!(maker, taker, Event::LockFinality(received.id));

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    assert!(matches!(taker_cfd.state, CfdState::Open { .. }));
    assert!(matches!(maker_cfd.state, CfdState::Open { .. }));
    (maker, taker, received.id)
}
