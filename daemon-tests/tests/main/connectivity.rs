use daemon::online_status::ConnectionStatus;
use daemon_tests::flow::next_with;
use daemon_tests::Maker;
use daemon_tests::MakerConfig;
use daemon_tests::Taker;
use daemon_tests::TakerConfig;
use otel_tests::otel_test;

#[otel_test]
async fn taker_notices_lack_of_maker() {
    let maker_config = MakerConfig::default();
    let maker = Maker::start(&maker_config).await;

    let taker_config = TakerConfig::default();
    let mut taker = Taker::start(&taker_config, maker.identity, maker.connect_addr.clone()).await;

    wait_next_connection_status_to_maker(&mut taker, ConnectionStatus::Online).await;

    drop(maker);

    wait_next_connection_status_to_maker(&mut taker, ConnectionStatus::Offline).await;

    let _maker = Maker::start(&maker_config).await;

    wait_next_connection_status_to_maker(&mut taker, ConnectionStatus::Online).await;
}

/// Wait indefinitely until the `taker`'s connection status to the maker is the one `expected` by
/// the caller.
async fn wait_next_connection_status_to_maker(taker: &mut Taker, expected: ConnectionStatus) {
    let is_expected = next_with(taker.maker_status_feed(), |actual| {
        (actual == expected).then_some(())
    })
    .await
    .is_ok();

    assert!(is_expected)
}

// TODO: Reinstate maker_notices_lack_of_taker by allowing to query Endpoint
