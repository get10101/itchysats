use daemon_tests::open_cfd;
use daemon_tests::settle_non_collaboratively;
use daemon_tests::start_both;
use daemon_tests::OpenCfdArgs;
use model::Position;
use otel_tests::otel_test;

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

    let order_id = open_cfd(&mut taker, &mut maker, open_cfd_args.clone()).await;
    // Taker initiates force-closing
    taker.system.commit(order_id).await.unwrap();

    settle_non_collaboratively(
        &mut taker,
        &mut maker,
        order_id,
        &open_cfd_args.oracle_data.settlement_attestation(),
    )
    .await;
}
