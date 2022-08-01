use daemon::online_status::ConnectionStatus;
use daemon_tests::flow::next;
use daemon_tests::Maker;
use daemon_tests::MakerConfig;
use daemon_tests::Taker;
use daemon_tests::TakerConfig;
use otel_tests::otel_test;
use std::time::Duration;
use tokio_extras::time::sleep;

#[otel_test]
async fn taker_notices_lack_of_maker() {
    let maker_config = MakerConfig::default()
        .with_dedicated_port(35123)
        .with_dedicated_libp2p_port(35124); // set fixed ports so the taker can reconnect
    let maker = Maker::start(&maker_config).await;

    let taker_config = TakerConfig::default();
    let mut taker = Taker::start(
        &taker_config,
        maker.listen_addr,
        maker.identity,
        maker.connect_addr.clone(),
    )
    .await;

    sleep(Duration::from_secs(5)).await; // wait a bit until taker notices change

    assert_eq!(
        ConnectionStatus::Online,
        next(taker.maker_status_feed()).await.unwrap()
    );

    drop(maker);

    sleep(Duration::from_secs(5)).await; // wait a bit until taker notices change

    assert_eq!(
        ConnectionStatus::Offline,
        next(taker.maker_status_feed()).await.unwrap(),
    );

    let _maker = Maker::start(&maker_config).await;

    sleep(Duration::from_secs(5)).await; // wait a bit until taker notices change

    assert_eq!(
        ConnectionStatus::Online,
        next(taker.maker_status_feed()).await.unwrap(),
    );
}

// TODO: Reinstate maker_notices_lack_of_taker by allowing to query Endpoint
