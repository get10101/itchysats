use daemon::projection::CfdState;
use daemon_tests::confirm;
use daemon_tests::expire;
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
async fn open_cfd_is_refunded() {
    let oracle_data = OliviaData::example_0();
    let (mut maker, mut taker, order_id, _) =
        start_from_open_cfd_state(oracle_data.announcement(), Position::Short).await;
    confirm!(commit transaction, order_id, maker, taker);

    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);

    expire!(refund timelock, order_id, maker, taker);
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_id, maker, taker, CfdState::PendingRefund);

    confirm!(refund transaction, order_id, maker, taker);
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_id, maker, taker, CfdState::Refunded);
}
