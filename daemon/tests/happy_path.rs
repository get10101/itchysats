use crate::harness::flow::{is_next_none, next_cfd, next_order, next_some};
use crate::harness::{assert_is_same_order, dummy_new_order, init_tracing, start_both};
use daemon::model::cfd::CfdState;
use daemon::model::Usd;
use maia::secp256k1_zkp::schnorrsig;
use rust_decimal_macros::dec;
mod harness;

#[tokio::test]
async fn taker_receives_order_from_maker_on_publication() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    assert!(is_next_none(&mut taker.order_feed).await);

    maker.publish_order(dummy_new_order()).await;

    let (published, received) = tokio::join!(
        next_some(&mut maker.order_feed),
        next_some(&mut taker.order_feed)
    );

    assert_is_same_order(&published, &received);
}

#[tokio::test]
async fn taker_takes_order_and_maker_rejects() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    // TODO: Why is this needed? For the cfd stream it is not needed
    is_next_none(&mut taker.order_feed).await;

    maker.publish_order(dummy_new_order()).await;

    let (_, received) = next_order(&mut maker.order_feed, &mut taker.order_feed).await;

    taker.take_order(received.clone(), Usd::new(dec!(10))).await;

    let (taker_cfd, maker_cfd) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;
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

    let (taker_cfd, maker_cfd) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;
    // TODO: More elaborate Cfd assertions
    assert_is_same_order(&taker_cfd.order, &received);
    assert_is_same_order(&maker_cfd.order, &received);
    assert!(matches!(taker_cfd.state, CfdState::Rejected { .. }));
    assert!(matches!(maker_cfd.state, CfdState::Rejected { .. }));
}
#[tokio::test]
#[cfg_attr(not(feature = "expensive_tests"), ignore)]
async fn taker_takes_order_and_maker_accepts_and_contract_setup() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    is_next_none(&mut taker.order_feed).await;

    maker.publish_order(dummy_new_order()).await;

    let (_, received) = next_order(&mut maker.order_feed, &mut taker.order_feed).await;

    taker.take_order(received.clone(), Usd::new(dec!(5))).await;
    let (_, _) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;

    maker.mocks.mock_oracle_annoucement().await;
    taker.mocks.mock_oracle_annoucement().await;

    maker.mocks.mock_monitor_oracle_attestation().await;

    maker.mocks.mock_party_params().await;
    taker.mocks.mock_party_params().await;

    maker.accept_take_request(received.clone()).await;

    let (taker_cfd, maker_cfd) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;
    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::ContractSetup { .. }));
    assert!(matches!(maker_cfd.state, CfdState::ContractSetup { .. }));

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    let (taker_cfd, maker_cfd) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;
    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::PendingOpen { .. }));
    assert!(matches!(maker_cfd.state, CfdState::PendingOpen { .. }));
}
