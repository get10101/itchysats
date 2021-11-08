use crate::harness::bdk::dummy_tx_id;
use crate::harness::flow::{is_next_none, next, next_cfd, next_order, next_some};
use crate::harness::mocks::oracle::dummy_announcement;
use crate::harness::mocks::wallet::build_party_params;
use crate::harness::{assert_is_same_order, dummy_new_order, init_tracing, start_both};
use daemon::model::cfd::{CfdState, UpdateCfdProposal};
use daemon::model::Usd;
use daemon::monitor;
use maia::secp256k1_zkp::schnorrsig;
use rust_decimal_macros::dec;
use tokio::sync::MutexGuard;
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

    maker
        .mocks
        .oracle()
        .await
        .expect_get_announcement()
        .returning(|_| Some(dummy_announcement()));

    taker
        .mocks
        .oracle()
        .await
        .expect_get_announcement()
        .returning(|_| Some(dummy_announcement()));

    #[allow(clippy::redundant_closure)] // clippy is in the wrong here
    maker
        .mocks
        .wallet()
        .await
        .expect_build_party_params()
        .returning(|msg| build_party_params(msg));

    #[allow(clippy::redundant_closure)] // clippy is in the wrong here
    taker
        .mocks
        .wallet()
        .await
        .expect_build_party_params()
        .returning(|msg| build_party_params(msg));

    #[allow(clippy::redundant_closure)] // clippy is in the wrong here
    maker
        .mocks
        .oracle()
        .await
        .expect_monitor_attestation()
        .return_const(());

    #[allow(clippy::redundant_closure)] // clippy is in the wrong here
    taker
        .mocks
        .oracle()
        .await
        .expect_monitor_attestation()
        .return_const(());

    #[allow(clippy::redundant_closure)] // clippy is in the wrong here
    maker
        .mocks
        .monitor()
        .await
        .expect_start_monitoring()
        .return_const(());

    #[allow(clippy::redundant_closure)] // clippy is in the wrong here
    taker
        .mocks
        .monitor()
        .await
        .expect_start_monitoring()
        .return_const(());

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

    mock_wallet_sign_and_broadcast(&mut maker.mocks.wallet().await);
    mock_wallet_sign_and_broadcast(&mut taker.mocks.wallet().await);

    let (taker_cfd, maker_cfd) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;

    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::PendingOpen { .. }));
    assert!(matches!(maker_cfd.state, CfdState::PendingOpen { .. }));

    taker.trigger_auto_rollover().await;

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

    // 1.1 second sleep will ensure the rollover offset time has elapsed (rollover_offset *
    // settlement_interval = 1 second)
    tokio::time::sleep(Duration::from_millis(1100)).await;

    taker.trigger_auto_rollover().await;

    loop {
        let update_proposals = next(&mut maker.update_proposals).await;
        if let Some(update_proposal) = update_proposals.get(&maker_cfd.order.id) {
            assert!(matches!(
                update_proposal,
                UpdateCfdProposal::RollOverProposal { .. }
            ));
            break;
        }
    }

    maker.accept_rollover(maker_cfd.order).await;

    let (taker_cfd_rolled_over, maker_cfd_rolled_over) =
        next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;

    assert!(matches!(taker_cfd_rolled_over.state, CfdState::Open { .. }));
    assert!(matches!(maker_cfd_rolled_over.state, CfdState::Open { .. }));

    assert_ne!(
        taker_cfd.state.get_transition_timestamp(),
        taker_cfd_rolled_over.state.get_transition_timestamp()
    );
}

// Helper function setting up a "happy path" wallet mock
fn mock_wallet_sign_and_broadcast(wallet: &mut MutexGuard<'_, harness::mocks::wallet::MockWallet>) {
    let mut seq = mockall::Sequence::new();
    wallet
        .expect_sign()
        .times(1)
        .returning(|sign| Ok(sign.psbt))
        .in_sequence(&mut seq);
    wallet
        .expect_broadcast()
        .times(1)
        .returning(|_| Ok(dummy_tx_id()))
        .in_sequence(&mut seq);
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
