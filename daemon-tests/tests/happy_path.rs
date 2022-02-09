use daemon::bdk::bitcoin::Amount;
use daemon::connection::ConnectionStatus;
use daemon::model::cfd::calculate_long_margin;
use daemon::model::cfd::OrderId;
use daemon::model::Identity;
use daemon::model::Usd;
use daemon::oracle;
use daemon::projection::CfdOrder;
use daemon::projection::CfdState;
use daemon_tests::deliver_event;
use daemon_tests::dummy_new_order;
use daemon_tests::dummy_quote;
use daemon_tests::flow::is_next_none;
use daemon_tests::flow::next;
use daemon_tests::flow::next_order;
use daemon_tests::flow::next_with;
use daemon_tests::flow::one_cfd_with_state;
use daemon_tests::init_tracing;
use daemon_tests::maia::OliviaData;
use daemon_tests::mocks::oracle::dummy_wrong_attestation;
use daemon_tests::start_both;
use daemon_tests::wait_next_state;
use daemon_tests::Maker;
use daemon_tests::MakerConfig;
use daemon_tests::Taker;
use daemon_tests::TakerConfig;
use rust_decimal_macros::dec;
use std::time::Duration;
use tokio::time::sleep;

macro_rules! confirm {
    (lock transaction, $id:expr, $maker:expr, $taker:expr) => {
        $maker
            .mocks
            .monitor()
            .await
            .confirm_lock_transaction($id)
            .await;
        $taker
            .mocks
            .monitor()
            .await
            .confirm_lock_transaction($id)
            .await;
    };
    (commit transaction, $id:expr, $maker:expr, $taker:expr) => {
        $maker
            .mocks
            .monitor()
            .await
            .confirm_commit_transaction($id)
            .await;
        $taker
            .mocks
            .monitor()
            .await
            .confirm_commit_transaction($id)
            .await;
    };
    (refund transaction, $id:expr, $maker:expr, $taker:expr) => {
        $maker
            .mocks
            .monitor()
            .await
            .confirm_refund_transaction($id)
            .await;
        $taker
            .mocks
            .monitor()
            .await
            .confirm_refund_transaction($id)
            .await;
    };
    (close transaction, $id:expr, $maker:expr, $taker:expr) => {
        $maker
            .mocks
            .monitor()
            .await
            .confirm_close_transaction($id)
            .await;
        $taker
            .mocks
            .monitor()
            .await
            .confirm_close_transaction($id)
            .await;
    };
    (cet, $id:expr, $maker:expr, $taker:expr) => {
        $maker.mocks.monitor().await.confirm_cet($id).await;
        $taker.mocks.monitor().await.confirm_cet($id).await;
    };
}

macro_rules! expire {
    (cet timelock, $id:expr, $maker:expr, $taker:expr) => {
        $maker.mocks.monitor().await.expire_cet_timelock($id).await;
        $taker.mocks.monitor().await.expire_cet_timelock($id).await;
    };
    (refund timelock, $id:expr, $maker:expr, $taker:expr) => {
        $maker
            .mocks
            .monitor()
            .await
            .expire_refund_timelock($id)
            .await;
        $taker
            .mocks
            .monitor()
            .await
            .expire_refund_timelock($id)
            .await;
    };
}

#[tokio::test]
async fn taker_receives_order_from_maker_on_publication() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    assert!(is_next_none(taker.order_feed()).await.unwrap());

    maker.publish_order(dummy_new_order()).await;

    let (published, received) = next_order(maker.order_feed(), taker.order_feed())
        .await
        .unwrap();

    assert_eq_order(published, received);
}

fn assert_eq_order(mut published: CfdOrder, received: CfdOrder) {
    // align margin_per_parcel to be the long margin_per_parcel
    let long_margin_per_parcel =
        calculate_long_margin(published.price, published.parcel_size, published.leverage);
    published.margin_per_parcel = long_margin_per_parcel;

    assert_eq!(published, received);

    // Hard-coded to match the dummy_new_order()
    assert_eq!(received.opening_fee.unwrap(), Amount::from_sat(2));
    assert_eq!(received.funding_rate_hourly_percent, "0.00100");
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
    taker
        .system
        .take_offer(received.id, Usd::new(dec!(10)))
        .await
        .unwrap();

    wait_next_state!(received.id, maker, taker, CfdState::PendingSetup);

    maker.system.reject_order(received.id).await.unwrap();

    wait_next_state!(received.id, maker, taker, CfdState::Rejected);
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

    taker.mocks.mock_oracle_announcement().await;
    maker.mocks.mock_oracle_announcement().await;

    taker
        .system
        .take_offer(received.id, Usd::new(dec!(5)))
        .await
        .unwrap();
    wait_next_state!(received.id, maker, taker, CfdState::PendingSetup);

    maker.mocks.mock_party_params().await;
    taker.mocks.mock_party_params().await;

    maker.mocks.mock_oracle_monitor_attestation().await;
    taker.mocks.mock_oracle_monitor_attestation().await;

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    maker.system.accept_order(received.id).await.unwrap();
    wait_next_state!(received.id, maker, taker, CfdState::ContractSetup);

    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(received.id, maker, taker, CfdState::PendingOpen);

    confirm!(lock transaction, received.id, maker, taker);
    wait_next_state!(received.id, maker, taker, CfdState::Open);
}

#[tokio::test]
async fn collaboratively_close_an_open_cfd() {
    let _guard = init_tracing();
    let (mut maker, mut taker, order_id) =
        start_from_open_cfd_state(OliviaData::example_0().announcement()).await;

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

#[tokio::test]
async fn force_close_an_open_cfd() {
    let _guard = init_tracing();
    let oracle_data = OliviaData::example_0();
    let (mut maker, mut taker, order_id) =
        start_from_open_cfd_state(oracle_data.announcement()).await;

    // Taker initiates force-closing
    taker.system.commit(order_id).await.unwrap();

    confirm!(commit transaction, order_id, maker, taker);
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);

    // After CetTimelockExpired, we're only waiting for attestation
    expire!(cet timelock, order_id, maker, taker);

    // Delivering the wrong attestation does not move state to `PendingCet`
    deliver_event!(maker, taker, dummy_wrong_attestation());
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);

    // Delivering correct attestation moves the state `PendingCet`
    deliver_event!(maker, taker, oracle_data.attestation());
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_id, maker, taker, CfdState::PendingCet);

    confirm!(cet, order_id, maker, taker);
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_id, maker, taker, CfdState::Closed);
}

#[tokio::test]
async fn rollover_an_open_cfd() {
    let _guard = init_tracing();
    let oracle_data = OliviaData::example_0();
    let (mut maker, mut taker, order_id) =
        start_from_open_cfd_state(oracle_data.announcement()).await;

    // Maker needs to have an active offer in order to accept rollover
    maker.publish_order(dummy_new_order()).await;

    taker.trigger_rollover(order_id).await;

    wait_next_state!(
        order_id,
        maker,
        taker,
        CfdState::IncomingRolloverProposal,
        CfdState::OutgoingRolloverProposal
    );

    maker.system.accept_rollover(order_id).await.unwrap();

    wait_next_state!(order_id, maker, taker, CfdState::ContractSetup);
    wait_next_state!(order_id, maker, taker, CfdState::Open);
}

#[tokio::test]
async fn maker_rejects_rollover_of_open_cfd() {
    let _guard = init_tracing();
    let oracle_data = OliviaData::example_0();
    let (mut maker, mut taker, order_id) =
        start_from_open_cfd_state(oracle_data.announcement()).await;

    taker.trigger_rollover(order_id).await;

    wait_next_state!(
        order_id,
        maker,
        taker,
        CfdState::IncomingRolloverProposal,
        CfdState::OutgoingRolloverProposal
    );

    maker.system.reject_rollover(order_id).await.unwrap();

    wait_next_state!(order_id, maker, taker, CfdState::Open);
}

#[tokio::test]
async fn maker_rejects_rollover_after_commit_finality() {
    let _guard = init_tracing();
    let oracle_data = OliviaData::example_0();
    let (mut maker, mut taker, order_id) =
        start_from_open_cfd_state(oracle_data.announcement()).await;

    taker.mocks.mock_latest_quote(Some(dummy_quote())).await;
    maker.mocks.mock_latest_quote(Some(dummy_quote())).await;
    next_with(taker.quote_feed(), |q| q).await.unwrap(); // if quote is available on feed, it propagated through the system

    taker.trigger_rollover(order_id).await;

    wait_next_state!(
        order_id,
        maker,
        taker,
        CfdState::IncomingRolloverProposal,
        CfdState::OutgoingRolloverProposal
    );

    confirm!(commit transaction, order_id, maker, taker);
    // Cfd would be in "OpenCommitted" if it wasn't for the rollover

    maker.system.reject_rollover(order_id).await.unwrap();

    // After rejecting rollover, we should display where we were before the
    // rollover attempt
    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);
}

#[tokio::test]
async fn open_cfd_is_refunded() {
    let _guard = init_tracing();
    let oracle_data = OliviaData::example_0();
    let (mut maker, mut taker, order_id) =
        start_from_open_cfd_state(oracle_data.announcement()).await;

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

#[tokio::test]
async fn taker_notices_lack_of_maker() {
    let short_interval = Duration::from_secs(1);

    let _guard = init_tracing();

    let maker_config = MakerConfig::default()
        .with_heartbeat_interval(short_interval)
        .with_dedicated_port(35123); // set a fixed port so the taker can reconnect
    let maker = Maker::start(&maker_config).await;

    let taker_config = TakerConfig::default().with_heartbeat_interval(short_interval);
    let mut taker = Taker::start(&taker_config, maker.listen_addr, maker.identity).await;

    assert_eq!(
        ConnectionStatus::Online,
        next(taker.maker_status_feed()).await.unwrap()
    );

    std::mem::drop(maker);

    sleep(taker_config.heartbeat_interval).await;

    assert_eq!(
        ConnectionStatus::Offline { reason: None },
        next(taker.maker_status_feed()).await.unwrap(),
    );

    let _maker = Maker::start(&maker_config).await;

    sleep(taker_config.heartbeat_interval).await;

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
    let mut maker = Maker::start(&MakerConfig::default()).await;
    let mut taker = Taker::start(&TakerConfig::default(), maker.listen_addr, maker.identity).await;

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

    taker
        .system
        .take_offer(received.id, Usd::new(dec!(5)))
        .await
        .unwrap();
    wait_next_state!(received.id, maker, taker, CfdState::PendingSetup);

    maker.mocks.mock_party_params().await;
    taker.mocks.mock_party_params().await;

    maker.mocks.mock_oracle_monitor_attestation().await;
    taker.mocks.mock_oracle_monitor_attestation().await;

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    maker.system.accept_order(received.id).await.unwrap();
    wait_next_state!(received.id, maker, taker, CfdState::ContractSetup);

    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(received.id, maker, taker, CfdState::PendingOpen);

    confirm!(lock transaction, received.id, maker, taker);
    wait_next_state!(received.id, maker, taker, CfdState::Open);

    (maker, taker, received.id)
}
