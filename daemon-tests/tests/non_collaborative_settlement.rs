use daemon::projection::CfdState;
use daemon_tests::confirm;
use daemon_tests::expire;
use daemon_tests::flow::next_with;
use daemon_tests::flow::one_cfd_with_state;
use daemon_tests::mocks::oracle::dummy_wrong_attestation;
use daemon_tests::open_cfd;
use daemon_tests::simulate_attestation;
use daemon_tests::start_both;
use daemon_tests::wait_next_state;
use daemon_tests::OpenCfdArgs;
use model::Position;
use otel_tests::otel_test;
use std::time::Duration;
use tokio_extras::time::sleep;

#[otel_test]
async fn force_close_an_open_cfd_maker_going_short() {
    force_close_open_cfd(Position::Short).await;
}

#[otel_test]
async fn force_close_an_open_cfd_maker_going_long() {
    force_close_open_cfd(Position::Long).await;
}

async fn force_close_open_cfd(position_maker: Position) {
    let (mut maker, mut taker) = start_both().await;

    let open_cfd_args = OpenCfdArgs {
        position_maker,
        ..Default::default()
    };

    let (order_id, _) = open_cfd(&mut taker, &mut maker, open_cfd_args.clone()).await;
    // Taker initiates force-closing
    taker.system.commit(order_id).await.unwrap();

    confirm!(commit transaction, order_id, maker, taker);
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);

    // After CetTimelockExpired, we're only waiting for attestation
    expire!(cet timelock, order_id, maker, taker);

    // Delivering the wrong attestation does not move state to `PendingCet`
    simulate_attestation!(taker, maker, order_id, dummy_wrong_attestation());

    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);

    // Delivering correct attestation moves the state `PendingCet`
    simulate_attestation!(
        taker,
        maker,
        order_id,
        open_cfd_args.oracle_data.settlement_attestation()
    );

    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_id, maker, taker, CfdState::PendingCet);

    confirm!(cet, order_id, maker, taker);
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_id, maker, taker, CfdState::Closed);
}
