use crate::harness::dummy_new_order;
use crate::harness::flow::is_next_none;
use crate::harness::flow::next;
use crate::harness::flow::next_cfd;
use crate::harness::flow::next_order;
use crate::harness::flow::next_some;
use crate::harness::init_tracing;
use crate::harness::maia::OliviaData;
use crate::harness::mocks::oracle::dummy_wrong_attestation;
use crate::harness::start_both;
use crate::harness::Maker;
use crate::harness::MakerConfig;
use crate::harness::Taker;
use crate::harness::TakerConfig;
use daemon::connection::ConnectionStatus;
use daemon::model::cfd::OrderId;
use daemon::model::Identity;
use daemon::model::Usd;
use daemon::monitor::Event;
use daemon::oracle;
use daemon::projection::CfdState;
use maia::secp256k1_zkp::schnorrsig;
use rust_decimal_macros::dec;
use std::time::Duration;
use tokio::time::sleep;
mod harness;

/// Assert the next state of the single cfd present at both maker and taker
macro_rules! assert_next_state {
    ($state:expr, $maker:expr, $taker:expr, $id:expr) => {
        // TODO: Allow fetching cfd with the specified id if there is more than
        // one on the cfd feed
        let (taker_cfd, maker_cfd) = next_cfd($taker.cfd_feed(), $maker.cfd_feed())
            .await
            .unwrap();
        assert_eq!(
            taker_cfd.order_id, maker_cfd.order_id,
            "order id mismatch between maker and taker"
        );
        assert_eq!(
            taker_cfd.state, maker_cfd.state,
            "cfd state mismatch between maker and taker"
        );
        assert_eq!(taker_cfd.order_id, $id, "unexpected order id in the taker");
        assert_eq!(maker_cfd.order_id, $id, "unexpected order id in the maker");
        assert_eq!(taker_cfd.state, $state, "unexpected cfd state in the taker");
        assert_eq!(maker_cfd.state, $state, "unexpected cfd state in the maker");
    };
}

#[tokio::test]
async fn taker_receives_order_from_maker_on_publication() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    assert!(is_next_none(taker.order_feed()).await.unwrap());

    maker.publish_order(dummy_new_order()).await;

    let (published, received) =
        tokio::join!(next_some(maker.order_feed()), next_some(taker.order_feed()));

    assert_eq!(published.unwrap(), received.unwrap());
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

    taker.mocks.mock_oracle_announcement().await;
    maker.mocks.mock_oracle_announcement().await;
    taker.take_order(received.clone(), Usd::new(dec!(10))).await;

    assert_next_state!(CfdState::PendingSetup, maker, taker, received.id);

    maker.reject_take_request(received.clone()).await;

    assert_next_state!(CfdState::Rejected, maker, taker, received.id);
}

#[tokio::test]
#[ignore = "expensive, runs on CI"]
async fn taker_takes_order_and_maker_accepts_and_contract_setup() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    is_next_none(taker.order_feed()).await.unwrap();

    maker.publish_order(dummy_new_order()).await;

    let (_, received) = next_order(maker.order_feed(), taker.order_feed())
        .await
        .unwrap();

    taker.mocks.mock_oracle_announcement().await;
    maker.mocks.mock_oracle_announcement().await;

    taker.take_order(received.clone(), Usd::new(dec!(5))).await;
    assert_next_state!(CfdState::PendingSetup, maker, taker, received.id);

    maker.mocks.mock_party_params().await;
    taker.mocks.mock_party_params().await;

    maker.mocks.mock_monitor_oracle_attestation().await;
    taker.mocks.mock_monitor_oracle_attestation().await;

    maker.mocks.mock_oracle_monitor_attestation().await;
    taker.mocks.mock_oracle_monitor_attestation().await;

    maker.mocks.mock_monitor_start_monitoring().await;
    taker.mocks.mock_monitor_start_monitoring().await;

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    maker.accept_take_request(received.clone()).await;
    assert_next_state!(CfdState::ContractSetup, maker, taker, received.id);

    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    assert_next_state!(CfdState::PendingOpen, maker, taker, received.id);

    deliver_event!(maker, taker, Event::LockFinality(received.id));
    assert_next_state!(CfdState::Open, maker, taker, received.id);
}

#[tokio::test]
async fn collaboratively_close_an_open_cfd() {
    let _guard = init_tracing();
    let (mut maker, mut taker, order_id) =
        start_from_open_cfd_state(OliviaData::example_0().announcement()).await;

    taker.propose_settlement(order_id).await;

    let (taker_cfd, maker_cfd) = next_cfd(taker.cfd_feed(), maker.cfd_feed()).await.unwrap();
    assert_eq!(taker_cfd.state, CfdState::OutgoingSettlementProposal);
    assert_eq!(maker_cfd.state, CfdState::IncomingSettlementProposal);

    maker.mocks.mock_monitor_collaborative_settlement().await;
    taker.mocks.mock_monitor_collaborative_settlement().await;

    maker.accept_settlement_proposal(order_id).await;
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition

    assert_next_state!(CfdState::PendingClose, maker, taker, order_id);

    deliver_event!(maker, taker, Event::CloseFinality(order_id));

    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition

    assert_next_state!(CfdState::Closed, maker, taker, order_id);
}

#[tokio::test]
async fn force_close_an_open_cfd() {
    let _guard = init_tracing();
    let oracle_data = OliviaData::example_0();
    let (mut maker, mut taker, order_id) =
        start_from_open_cfd_state(oracle_data.announcement()).await;

    // Taker initiates force-closing
    taker.force_close(order_id).await;

    deliver_event!(maker, taker, Event::CommitFinality(order_id));
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    assert_next_state!(CfdState::OpenCommitted, maker, taker, order_id);

    // After CetTimelockExpired, we're only waiting for attestation
    deliver_event!(maker, taker, Event::CetTimelockExpired(order_id));

    // Delivering the wrong attestation does not move state to `PendingCet`
    deliver_event!(maker, taker, dummy_wrong_attestation());
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    assert_next_state!(CfdState::OpenCommitted, maker, taker, order_id);

    // Delivering correct attestation moves the state `PendingCet`
    deliver_event!(maker, taker, oracle_data.attestation());
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    assert_next_state!(CfdState::PendingCet, maker, taker, order_id);

    deliver_event!(maker, taker, Event::CetFinality(order_id));
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    assert_next_state!(CfdState::Closed, maker, taker, order_id);
}

#[tokio::test]
async fn taker_notices_lack_of_maker() {
    let short_interval = Duration::from_secs(1);

    let _guard = init_tracing();

    let maker_config = MakerConfig::default()
        .with_heartbeat_interval(short_interval)
        .with_dedicated_port(35123); // set a fixed port so the taker can reconnect
    let maker = Maker::start(&maker_config).await;

    let taker_config = TakerConfig::default().with_heartbeat_timeout(short_interval * 2);
    let mut taker = Taker::start(&taker_config, maker.listen_addr, maker.identity).await;

    assert_eq!(
        ConnectionStatus::Online,
        next(taker.maker_status_feed()).await.unwrap()
    );

    std::mem::drop(maker);

    sleep(taker_config.heartbeat_timeout).await;

    assert_eq!(
        ConnectionStatus::Offline { reason: None },
        next(taker.maker_status_feed()).await.unwrap(),
    );

    let _maker = Maker::start(&maker_config).await;

    sleep(taker_config.heartbeat_timeout).await;

    assert_eq!(
        ConnectionStatus::Online,
        next(taker.maker_status_feed()).await.unwrap(),
    );
}

#[tokio::test]
async fn maker_notices_lack_of_taker() {
    let _guard = init_tracing();

    let (mut maker, taker) = start_both().await;
    assert_eq!(
        vec![taker.id],
        next(maker.connected_takers_feed()).await.unwrap()
    );

    std::mem::drop(taker);

    assert_eq!(
        Vec::<Identity>::new(),
        next(maker.connected_takers_feed()).await.unwrap()
    );
}

/// Hide the implementation detail of arriving at the Cfd open state.
/// Useful when reading tests that should start at this point.
/// For convenience, returns also OrderId of the opened Cfd.
/// `announcement` is used during Cfd's creation.
async fn start_from_open_cfd_state(announcement: oracle::Announcement) -> (Maker, Taker, OrderId) {
    let heartbeat_interval = Duration::from_secs(60);
    let mut maker =
        Maker::start(&MakerConfig::default().with_heartbeat_interval(heartbeat_interval)).await;
    let mut taker = Taker::start(
        &TakerConfig::default().with_heartbeat_timeout(heartbeat_interval * 2),
        maker.listen_addr,
        maker.identity,
    )
    .await;

    is_next_none(taker.order_feed()).await.unwrap();

    maker.publish_order(dummy_new_order()).await;

    let (_, received) = next_order(maker.order_feed(), taker.order_feed())
        .await
        .unwrap();

    taker
        .mocks
        .mock_oracle_announcement_with(announcement.clone())
        .await;
    maker
        .mocks
        .mock_oracle_announcement_with(announcement)
        .await;

    taker.take_order(received.clone(), Usd::new(dec!(5))).await;
    assert_next_state!(CfdState::PendingSetup, maker, taker, received.id);

    maker.mocks.mock_party_params().await;
    taker.mocks.mock_party_params().await;

    maker.mocks.mock_monitor_oracle_attestation().await;
    taker.mocks.mock_monitor_oracle_attestation().await;

    maker.mocks.mock_oracle_monitor_attestation().await;
    taker.mocks.mock_oracle_monitor_attestation().await;

    maker.mocks.mock_monitor_start_monitoring().await;
    taker.mocks.mock_monitor_start_monitoring().await;

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    maker.accept_take_request(received.clone()).await;
    assert_next_state!(CfdState::ContractSetup, maker, taker, received.id);

    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    assert_next_state!(CfdState::PendingOpen, maker, taker, received.id);

    deliver_event!(maker, taker, Event::LockFinality(received.id));
    assert_next_state!(CfdState::Open, maker, taker, received.id);

    (maker, taker, received.id)
}
