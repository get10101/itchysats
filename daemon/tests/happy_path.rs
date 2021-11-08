use crate::harness::flow::{is_next_none, next_cfd, next_order, next_some};
use crate::harness::{assert_is_same_order, dummy_new_order, init_tracing, start_both};
use daemon::model::cfd::{CfdState, UpdateCfdProposal};
use daemon::model::Usd;
use daemon::monitor;
use maia::secp256k1_zkp::schnorrsig;
use rust_decimal_macros::dec;
use tokio::time::Duration;

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
async fn taker_proposes_rollover_and_maker_accepts_rollover() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    is_next_none(&mut taker.order_feed).await;

    maker.publish_order(dummy_new_order()).await;

    let (_, received) = next_order(&mut maker.order_feed, &mut taker.order_feed).await;

    taker.take_order(received.clone(), Usd::new(dec!(5))).await;
    let (_, _) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;

    maker.accept_take_request(received.clone()).await;

    let (taker_cfd, maker_cfd) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;
    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::ContractSetup { .. }));
    assert!(matches!(maker_cfd.state, CfdState::ContractSetup { .. }));

    taker.mocks.mock_wallet_sign_and_broadcast().await;
    maker.mocks.mock_wallet_sign_and_broadcast().await;

    let (taker_cfd, maker_cfd) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;

    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::PendingOpen { .. }));
    assert!(matches!(maker_cfd.state, CfdState::PendingOpen { .. }));

    maker
        .cfd_actor_addr
        .send(monitor::Event::LockFinality(maker_cfd.order.id))
        .await
        .unwrap();
    taker
        .cfd_actor_addr
        .send(monitor::Event::LockFinality(taker_cfd.order.id))
        .await
        .unwrap();

    let (taker_cfd_open_state, maker_cfd_open_state) =
        next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;

    assert!(matches!(taker_cfd_open_state.state, CfdState::Open { .. }));
    assert!(matches!(maker_cfd_open_state.state, CfdState::Open { .. }));

    // 1st rollover

    // 1.1 second sleep will ensure the rollover offset time has elapsed (rollover_offset *
    // settlement_interval = 1 second)
    tokio::time::sleep(Duration::from_millis(1100)).await;

    taker.trigger_auto_rollover().await;

    let update_cfd_proposal = maker
        .get_update_cfd_proposal(&maker_cfd.order.id)
        .await
        .expect("update cfd proposal received by maker");

    assert!(matches!(
        update_cfd_proposal,
        UpdateCfdProposal::RollOverProposal { .. }
    ));

    maker.accept_rollover(maker_cfd.order).await;

    let (taker_cfd_rolled_over_1, maker_cfd_rolled_over_1) =
        next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;

    assert!(matches!(
        taker_cfd_rolled_over_1.state,
        CfdState::Open { .. }
    ));
    assert!(matches!(
        maker_cfd_rolled_over_1.state,
        CfdState::Open { .. }
    ));

    assert_ne!(
        taker_cfd.state.get_transition_timestamp(),
        taker_cfd_rolled_over_1.state.get_transition_timestamp()
    );

    // 2nd rollover to test consecutive rollover

    // 1.1 second sleep will ensure the rollover offset time has elapsed (rollover_offset *
    // settlement_interval = 1 second)
    tokio::time::sleep(Duration::from_millis(1100)).await;

    taker.trigger_auto_rollover().await;

    let update_cfd_proposal = maker
        .get_update_cfd_proposal(&maker_cfd_rolled_over_1.order.id)
        .await
        .expect("update cfd proposal received by maker");

    assert!(matches!(
        update_cfd_proposal,
        UpdateCfdProposal::RollOverProposal { .. }
    ));

    maker.accept_rollover(maker_cfd_rolled_over_1.order).await;

    let (taker_cfd_rolled_over_2, maker_cfd_rolled_over_2) =
        next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;

    assert!(matches!(
        taker_cfd_rolled_over_2.state,
        CfdState::Open { .. }
    ));
    assert!(matches!(
        maker_cfd_rolled_over_2.state,
        CfdState::Open { .. }
    ));

    assert_ne!(
        taker_cfd_rolled_over_1.state.get_transition_timestamp(),
        taker_cfd_rolled_over_2.state.get_transition_timestamp()
    );
}

#[tokio::test]
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
