use crate::harness::flow::{is_next_none, next, next_cfd, next_order, next_some};
use crate::harness::{
    assert_is_same_order, dummy_new_order, init_open_cfd, init_tracing, start_both,
    HEARTBEAT_INTERVAL_FOR_TEST,
};
use anyhow::Result;
use daemon::connection::ConnectionStatus;
use daemon::model::cfd::{CfdState, UpdateCfdProposal};
use daemon::model::Usd;
use daemon::monitor;
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

    let (taker_cfd_open_state, maker_cfd_open_state) =
        next_cfd(&mut taker.cfd_feed(), &mut maker.cfd_feed())
            .await
            .unwrap();

    assert!(matches!(taker_cfd_open_state.state, CfdState::Open { .. }));
    assert!(matches!(maker_cfd_open_state.state, CfdState::Open { .. }));
}

#[tokio::test]
async fn taker_proposes_rollover_and_maker_accepts_rollover() -> Result<()> {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;
    let (taker_cfd_open_state, maker_cfd_open_state) =
        init_open_cfd(&mut maker, &mut taker).await?;

    // First rollover
    taker
        .trigger_propose_rollover(taker_cfd_open_state.order.id)
        .await;

    let update_cfd_proposal = maker
        .get_update_cfd_proposal(&maker_cfd_open_state.order.id)
        .await
        .expect("update cfd proposal received by maker");

    assert!(matches!(
        update_cfd_proposal,
        UpdateCfdProposal::RollOverProposal { .. }
    ));

    maker.accept_rollover(maker_cfd_open_state.order).await;

    let (taker_cfd_rolled_over_1, maker_cfd_rolled_over_1) =
        next_cfd(&mut taker.cfd_feed(), &mut maker.cfd_feed()).await?;

    assert!(matches!(
        taker_cfd_rolled_over_1.state,
        CfdState::Open { .. }
    ));
    assert!(matches!(
        maker_cfd_rolled_over_1.state,
        CfdState::Open { .. }
    ));

    assert_ne!(
        taker_cfd_open_state.state.get_transition_timestamp(),
        taker_cfd_rolled_over_1.state.get_transition_timestamp()
    );

    // We need to sleep for at least 1 second to ensure the rolled-over cfd has a different
    // transition timestamp
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    // Second rollover to test consecutive rollovers
    taker
        .trigger_propose_rollover(taker_cfd_rolled_over_1.order.id)
        .await;

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
        next_cfd(&mut taker.cfd_feed(), &mut maker.cfd_feed()).await?;

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

    Ok(())
}

#[tokio::test]
async fn taker_notices_lack_of_maker() {
    let _guard = init_tracing();

    let (maker, mut taker) = start_both().await;
    assert_eq!(
        ConnectionStatus::Online,
        next(taker.maker_status_feed()).await.unwrap()
    );

    std::mem::drop(maker);

    sleep(HEARTBEAT_INTERVAL_FOR_TEST * 3).await;

    assert_eq!(
        ConnectionStatus::Offline,
        next(taker.maker_status_feed()).await.unwrap(),
    );
}
