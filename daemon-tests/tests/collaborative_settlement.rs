use daemon::projection::CfdState;
use daemon_tests::confirm;
use daemon_tests::dummy_quote;
use daemon_tests::flow::next_with;
use daemon_tests::flow::one_cfd_with_state;
use daemon_tests::maia::OliviaData;
use daemon_tests::start_from_open_cfd_state;
use daemon_tests::wait_next_state;
use model::Position;
use otel_tests::otel_test;
use std::time::Duration;
use tokio_extras::time::sleep;

#[otel_test]
async fn collaboratively_close_an_open_cfd_maker_going_short() {
    collaboratively_close_an_open_cfd(Position::Short).await;
}

#[otel_test]
async fn collaboratively_close_an_open_cfd_maker_going_long() {
    collaboratively_close_an_open_cfd(Position::Long).await;
}

async fn collaboratively_close_an_open_cfd(maker_position: Position) {
    let (mut maker, mut taker, order_id, _) =
        start_from_open_cfd_state(OliviaData::example_0().announcement(), maker_position).await;
    taker.mocks.mock_latest_quote(Some(dummy_quote())).await;
    maker.mocks.mock_latest_quote(Some(dummy_quote())).await;
    next_with(taker.quote_feed(), |q| q).await.unwrap(); // if quote is available on feed, it propagated through the system

    taker.system.propose_settlement(order_id).await.unwrap();

    wait_next_state!(
        order_id,
        maker,
        taker,
        CfdState::IncomingSettlementProposal,
        CfdState::OutgoingSettlementProposal
    );

    maker.system.accept_settlement(order_id).await.unwrap();
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition

    wait_next_state!(order_id, maker, taker, CfdState::PendingClose);
    confirm!(close transaction, order_id, maker, taker);

    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition

    wait_next_state!(order_id, maker, taker, CfdState::Closed);
}

#[otel_test]
async fn maker_rejects_collab_settlement_after_commit_finality() {
    let (mut maker, mut taker, order_id, _) =
        start_from_open_cfd_state(OliviaData::example_0().announcement(), Position::Short).await;
    taker.mocks.mock_latest_quote(Some(dummy_quote())).await;
    maker.mocks.mock_latest_quote(Some(dummy_quote())).await;
    next_with(taker.quote_feed(), |q| q).await.unwrap(); // if quote is available on feed, it propagated through the system

    taker.system.propose_settlement(order_id).await.unwrap();

    wait_next_state!(
        order_id,
        maker,
        taker,
        CfdState::IncomingSettlementProposal,
        CfdState::OutgoingSettlementProposal
    );

    confirm!(commit transaction, order_id, maker, taker);

    maker.system.reject_settlement(order_id).await.unwrap();
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition

    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);
}

#[otel_test]
async fn maker_accepts_collab_settlement_after_commit_finality() {
    let (mut maker, mut taker, order_id, _) =
        start_from_open_cfd_state(OliviaData::example_0().announcement(), Position::Short).await;
    taker.mocks.mock_latest_quote(Some(dummy_quote())).await;
    maker.mocks.mock_latest_quote(Some(dummy_quote())).await;
    next_with(taker.quote_feed(), |q| q).await.unwrap(); // if quote is available on feed, it propagated through the system

    taker.system.propose_settlement(order_id).await.unwrap();

    wait_next_state!(
        order_id,
        maker,
        taker,
        CfdState::IncomingSettlementProposal,
        CfdState::OutgoingSettlementProposal
    );

    confirm!(commit transaction, order_id, maker, taker);

    maker.system.accept_settlement(order_id).await.unwrap();
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition

    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);
}
