use crate::harness::flow::{is_next_none, next, next_cfd, next_order, next_some};
use crate::harness::{
    assert_is_same_order, dummy_new_order, init_tracing, start_both, Maker, MakerConfig, Taker,
    TakerConfig,
};
use daemon::connection::ConnectionStatus;
use daemon::model::cfd::CfdState;
use daemon::model::{TakerId, Usd};
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

    assert_is_same_order(&published.unwrap(), &received.unwrap());
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

    taker.take_order(received.clone(), Usd::new(dec!(10))).await;

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    assert_is_same_order(&taker_cfd.order, &received);
    assert_is_same_order(&maker_cfd.order, &received);
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
    assert_is_same_order(&taker_cfd.order, &received);
    assert_is_same_order(&maker_cfd.order, &received);
    assert!(matches!(taker_cfd.state, CfdState::Rejected { .. }));
    assert!(matches!(maker_cfd.state, CfdState::Rejected { .. }));
}

#[tokio::test]
async fn taker_takes_order_and_maker_accepts_and_contract_setup() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    is_next_none(taker.order_feed()).await.unwrap();

    maker.publish_order(dummy_new_order()).await;

    let (_, received) = next_order(maker.order_feed(), taker.order_feed())
        .await
        .unwrap();

    taker.take_order(received.clone(), Usd::new(dec!(5))).await;
    let (_, _) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();

    maker.mocks.mock_oracle_annoucement().await;
    taker.mocks.mock_oracle_annoucement().await;

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
async fn taker_notices_lack_of_maker() {
    let _guard = init_tracing();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

    let local_addr = listener.local_addr().unwrap();

    let maker_config = MakerConfig::default();

    let maker = Maker::start(&maker_config, listener).await;

    let taker_config = TakerConfig::default();

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
        vec![taker.id],
        next(maker.connected_takers_feed()).await.unwrap()
    );

    std::mem::drop(taker);

    assert_eq!(
        Vec::<TakerId>::new(),
        next(maker.connected_takers_feed()).await.unwrap()
    );
}
