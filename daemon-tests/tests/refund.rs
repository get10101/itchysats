use daemon::projection::CfdState;
use daemon_tests::confirm;
use daemon_tests::expire;
use daemon_tests::flow::next_with;
use daemon_tests::flow::one_cfd_with_state;
use daemon_tests::open_cfd;
use daemon_tests::start_both;
use daemon_tests::wait_next_state;
use daemon_tests::OpenCfdArgs;
use otel_tests::otel_test;

#[otel_test]
async fn open_cfd_is_refunded() {
    let (mut maker, mut taker) = start_both().await;
    let order_id = open_cfd(&mut taker, &mut maker, OpenCfdArgs::default()).await;
    confirm!(commit transaction, order_id, maker, taker);

    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);

    expire!(refund timelock, order_id, maker, taker);
    wait_next_state!(order_id, maker, taker, CfdState::PendingRefund);

    confirm!(refund transaction, order_id, maker, taker);
    wait_next_state!(order_id, maker, taker, CfdState::Refunded);
}
