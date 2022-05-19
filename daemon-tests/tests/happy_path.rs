use anyhow::Context;
use daemon::bdk::bitcoin::Amount;
use daemon::connection::ConnectionStatus;
use daemon::connection::MAX_RECONNECT_INTERVAL_SECONDS;
use daemon::projection::CfdOrder;
use daemon::projection::CfdState;
use daemon::projection::MakerOffers;
use daemon_tests::dummy_offer_params;
use daemon_tests::dummy_quote;
use daemon_tests::flow::is_next_offers_none;
use daemon_tests::flow::next;
use daemon_tests::flow::next_maker_offers;
use daemon_tests::flow::next_with;
use daemon_tests::flow::one_cfd_with_state;
use daemon_tests::init_tracing;
use daemon_tests::maia::OliviaData;
use daemon_tests::mocks::oracle::dummy_wrong_attestation;
use daemon_tests::simulate_attestation;
use daemon_tests::start_both;
use daemon_tests::wait_next_state;
use daemon_tests::Maker;
use daemon_tests::MakerConfig;
use daemon_tests::Taker;
use daemon_tests::TakerConfig;
use model::calculate_margin;
use model::olivia;
use model::Identity;
use model::Leverage;
use model::OrderId;
use model::Position;
use model::Usd;
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

    assert!(is_next_offers_none(taker.offers_feed()).await.unwrap());

    maker
        .set_offer_params(dummy_offer_params(Position::Short))
        .await;

    let (published, received) = next_maker_offers(maker.offers_feed(), taker.offers_feed())
        .await
        .unwrap();

    assert_eq_offers(published, received);
}

fn assert_eq_offers(published: MakerOffers, received: MakerOffers) {
    match (published.long, received.long) {
        (None, None) => (),
        (Some(published), Some(received)) => assert_eq_orders(published, received),
        (None, Some(_)) => panic!("Long orders did not match: Published None, received Some "),
        (Some(_), None) => panic!("Long orders did not match: Published Some, received None "),
    }

    match (published.short, received.short) {
        (None, None) => (),
        (Some(published), Some(received)) => assert_eq_orders(published, received),
        (None, Some(_)) => panic!("Short orders did not match: Published None, received Some "),
        (Some(_), None) => panic!("Short orders did not match: Published Some, received None "),
    }
}

fn assert_eq_orders(mut published: CfdOrder, received: CfdOrder) {
    // align margin_per_lot to be the long margin_per_lot
    let long_margin_per_lot = calculate_margin(published.price, published.lot_size, Leverage::TWO);
    published.margin_per_lot = long_margin_per_lot;

    // make sure that the initial funding fee per lot is flipped
    // note: we publish as maker and receive as taker, the funding fee is to be received by one
    // party and paid by the other
    assert_eq!(
        published.initial_funding_fee_per_lot,
        received.initial_funding_fee_per_lot * -1
    );
    // align initial_funding_fee_per_lot so we can assert on the order
    published.initial_funding_fee_per_lot = received.initial_funding_fee_per_lot;

    // align liquidation price so we can assert on the order
    published.liquidation_price = received.liquidation_price;

    assert_eq!(published, received);

    // Hard-coded to match the dummy_new_order()
    assert_eq!(received.opening_fee.unwrap(), Amount::from_sat(2));
    assert_eq!(received.funding_rate_hourly_percent, "0.00100");
}

#[tokio::test]
async fn taker_takes_order_and_maker_rejects() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    is_next_offers_none(taker.offers_feed()).await.unwrap();

    maker
        .set_offer_params(dummy_offer_params(Position::Short))
        .await;

    let (_, received) = next_maker_offers(maker.offers_feed(), taker.offers_feed())
        .await
        .unwrap();

    let order_id = received.short.unwrap().id;

    taker.mocks.mock_oracle_announcement().await;
    maker.mocks.mock_oracle_announcement().await;
    taker
        .system
        .take_offer(order_id, Usd::new(dec!(10)), Leverage::TWO)
        .await
        .unwrap();

    wait_next_state!(order_id, maker, taker, CfdState::PendingSetup);

    maker.system.reject_order(order_id).await.unwrap();

    wait_next_state!(order_id, maker, taker, CfdState::Rejected);
}

#[tokio::test]
async fn another_offer_is_automatically_created_after_taker_takes_order() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    is_next_offers_none(taker.offers_feed()).await.unwrap();

    maker
        .set_offer_params(dummy_offer_params(Position::Short))
        .await;

    let (_, received) = next_maker_offers(maker.offers_feed(), taker.offers_feed())
        .await
        .unwrap();

    let order_id_take = received.short.unwrap().id;

    taker.mocks.mock_oracle_announcement().await;
    maker.mocks.mock_oracle_announcement().await;
    taker
        .system
        .take_offer(order_id_take, Usd::new(dec!(10)), Leverage::TWO)
        .await
        .unwrap();

    // For the taker the offer is reset after taking it, so we can't take the same one twice
    is_next_offers_none(taker.offers_feed()).await.unwrap();

    let (maker_update, taker_update) = next_maker_offers(maker.offers_feed(), taker.offers_feed())
        .await
        .unwrap();

    let new_order_id_maker = maker_update.short.unwrap().id;
    let new_order_id_taker = taker_update.short.unwrap().id;

    assert_ne!(
        new_order_id_taker, order_id_take,
        "Another offer should be available, and it should have a different id than first one"
    );

    assert_ne!(
        new_order_id_maker, order_id_take,
        "Another offer should be available, and it should have a different id than first one"
    );

    assert_eq!(
        new_order_id_taker, new_order_id_maker,
        "Both parties have the same new id"
    )
}

#[tokio::test]
async fn taker_takes_order_and_maker_accepts_and_contract_setup() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    is_next_offers_none(taker.offers_feed()).await.unwrap();

    maker
        .set_offer_params(dummy_offer_params(Position::Short))
        .await;

    let (_, received) = next_maker_offers(maker.offers_feed(), taker.offers_feed())
        .await
        .unwrap();

    let order_id = received.short.unwrap().id;

    taker.mocks.mock_oracle_announcement().await;
    maker.mocks.mock_oracle_announcement().await;

    taker
        .system
        .take_offer(order_id, Usd::new(dec!(5)), Leverage::TWO)
        .await
        .unwrap();
    wait_next_state!(order_id, maker, taker, CfdState::PendingSetup);

    maker.mocks.mock_party_params().await;
    taker.mocks.mock_party_params().await;

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    maker.system.accept_order(order_id).await.unwrap();
    wait_next_state!(order_id, maker, taker, CfdState::ContractSetup);

    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_id, maker, taker, CfdState::PendingOpen);

    confirm!(lock transaction, order_id, maker, taker);
    wait_next_state!(order_id, maker, taker, CfdState::Open);
}

#[tokio::test]
async fn collaboratively_close_an_open_cfd_maker_going_short() {
    let _guard = init_tracing();
    collaboratively_close_an_open_cfd(Position::Short).await;
}

#[tokio::test]
async fn collaboratively_close_an_open_cfd_maker_going_long() {
    let _guard = init_tracing();
    collaboratively_close_an_open_cfd(Position::Long).await;
}

async fn collaboratively_close_an_open_cfd(maker_position: Position) {
    let (mut maker, mut taker, order_id) =
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

#[tokio::test]
async fn force_close_an_open_cfd_maker_going_short() {
    let _guard = init_tracing();
    force_close_open_cfd(Position::Short).await;
}

#[tokio::test]
async fn force_close_an_open_cfd_maker_going_long() {
    let _guard = init_tracing();
    force_close_open_cfd(Position::Long).await;
}

async fn force_close_open_cfd(maker_position: Position) {
    let oracle_data = OliviaData::example_0();
    let (mut maker, mut taker, order_id) =
        start_from_open_cfd_state(oracle_data.announcement(), maker_position).await;
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
    simulate_attestation!(taker, maker, order_id, oracle_data.attestation());

    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_id, maker, taker, CfdState::PendingCet);

    confirm!(cet, order_id, maker, taker);
    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_id, maker, taker, CfdState::Closed);
}

#[tokio::test]
async fn rollover_an_open_cfd_maker_going_short() {
    let _guard = init_tracing();
    rollover_an_open_cfd(Position::Short).await;
}

#[tokio::test]
async fn rollover_an_open_cfd_maker_going_long() {
    let _guard = init_tracing();
    rollover_an_open_cfd(Position::Long).await;
}

async fn rollover_an_open_cfd(maker_position: Position) {
    let oracle_data = OliviaData::example_0();
    let (mut maker, mut taker, order_id) =
        start_from_open_cfd_state(oracle_data.announcement(), maker_position).await;

    // Maker needs to have an active offer in order to accept rollover
    maker
        .set_offer_params(dummy_offer_params(maker_position))
        .await;

    taker.trigger_rollover(order_id, Some(maker.peer_id)).await;

    wait_next_state!(
        order_id,
        maker,
        taker,
        CfdState::IncomingRolloverProposal,
        CfdState::OutgoingRolloverProposal
    );

    maker.system.accept_rollover(order_id).await.unwrap();

    wait_next_state!(order_id, maker, taker, CfdState::RolloverSetup);
    wait_next_state!(order_id, maker, taker, CfdState::Open);
}

#[tokio::test]
async fn maker_rejects_rollover_of_open_cfd() {
    let _guard = init_tracing();
    let oracle_data = OliviaData::example_0();
    let (mut maker, mut taker, order_id) =
        start_from_open_cfd_state(oracle_data.announcement(), Position::Short).await;

    taker.trigger_rollover(order_id, Some(maker.peer_id)).await;

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
        start_from_open_cfd_state(oracle_data.announcement(), Position::Short).await;

    taker.mocks.mock_latest_quote(Some(dummy_quote())).await;
    maker.mocks.mock_latest_quote(Some(dummy_quote())).await;
    next_with(taker.quote_feed(), |q| q).await.unwrap(); // if quote is available on feed, it propagated through the system

    taker.trigger_rollover(order_id, None).await;

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
async fn maker_accepts_rollover_after_commit_finality() {
    let _guard = init_tracing();
    let oracle_data = OliviaData::example_0();
    let (mut maker, mut taker, order_id) =
        start_from_open_cfd_state(oracle_data.announcement(), Position::Short).await;

    taker.mocks.mock_latest_quote(Some(dummy_quote())).await;
    maker.mocks.mock_latest_quote(Some(dummy_quote())).await;
    next_with(taker.quote_feed(), |q| q).await.unwrap(); // if quote is available on feed, it propagated through the system

    taker.trigger_rollover(order_id, None).await;

    wait_next_state!(
        order_id,
        maker,
        taker,
        CfdState::IncomingRolloverProposal,
        CfdState::OutgoingRolloverProposal
    );

    confirm!(commit transaction, order_id, maker, taker);

    maker.system.accept_rollover(order_id).await.unwrap(); // This should fail

    wait_next_state!(
        order_id,
        maker,
        taker,
        // FIXME: Maker wrongly changes state even when rollover does not happen
        CfdState::RolloverSetup,
        CfdState::OpenCommitted
    );
}

#[tokio::test]
async fn maker_rejects_collab_settlement_after_commit_finality() {
    let _guard = init_tracing();
    let (mut maker, mut taker, order_id) =
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

    // After rejecting rollover, we should display where we were before the
    // rollover attempt
    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);
}

#[tokio::test]
async fn maker_accepts_collab_settlement_after_commit_finality() {
    let _guard = init_tracing();
    let (mut maker, mut taker, order_id) =
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

    // After rejecting rollover, we should display where we were before the
    // rollover attempt
    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);
}

#[tokio::test]
async fn open_cfd_is_refunded() {
    let _guard = init_tracing();
    let oracle_data = OliviaData::example_0();
    let (mut maker, mut taker, order_id) =
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

#[tokio::test]
async fn taker_notices_lack_of_maker() {
    let short_interval = Duration::from_secs(1);

    let _guard = init_tracing();

    let maker_config = MakerConfig::default()
        .with_heartbeat_interval(short_interval)
        .with_dedicated_port(35123); // set a fixed port so the taker can reconnect
    let maker = Maker::start(&maker_config).await;

    let taker_config = TakerConfig::default().with_heartbeat_interval(short_interval);
    let mut taker = Taker::start(
        &taker_config,
        maker.listen_addr,
        maker.identity,
        maker.peer_id,
    )
    .await;

    assert_eq!(
        ConnectionStatus::Online,
        next(taker.maker_status_feed()).await.unwrap()
    );

    drop(maker);

    sleep(taker_config.heartbeat_interval).await;

    assert_eq!(
        ConnectionStatus::Offline { reason: None },
        next(taker.maker_status_feed()).await.unwrap(),
    );

    let _maker = Maker::start(&maker_config).await;

    sleep(Duration::from_secs(MAX_RECONNECT_INTERVAL_SECONDS) + taker_config.heartbeat_interval)
        .await;

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

    drop(taker);

    assert_eq!(
        Vec::<Identity>::new(),
        next(maker.connected_takers_feed()).await.unwrap()
    );
}

/// Hide the implementation detail of arriving at the Cfd open state.
/// Useful when reading tests that should start at this point.
/// For convenience, returns also OrderId of the opened Cfd.
/// `announcement` is used during Cfd's creation.
async fn start_from_open_cfd_state(
    announcement: olivia::Announcement,
    position_maker: Position,
) -> (Maker, Taker, OrderId) {
    let mut maker = Maker::start(&MakerConfig::default()).await;
    let mut taker = Taker::start(
        &TakerConfig::default(),
        maker.listen_addr,
        maker.identity,
        maker.peer_id,
    )
    .await;

    is_next_offers_none(taker.offers_feed()).await.unwrap();

    maker
        .set_offer_params(dummy_offer_params(position_maker))
        .await;

    let (_, received) = next_maker_offers(maker.offers_feed(), taker.offers_feed())
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

    let order_to_take = match position_maker {
        Position::Short => received.short,
        Position::Long => received.long,
    }
    .context("Order for expected position not set")
    .unwrap();

    taker
        .system
        .take_offer(order_to_take.id, Usd::new(dec!(5)), Leverage::TWO)
        .await
        .unwrap();
    wait_next_state!(order_to_take.id, maker, taker, CfdState::PendingSetup);

    maker.mocks.mock_party_params().await;
    taker.mocks.mock_party_params().await;

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    maker.system.accept_order(order_to_take.id).await.unwrap();
    wait_next_state!(order_to_take.id, maker, taker, CfdState::ContractSetup);

    sleep(Duration::from_secs(5)).await; // need to wait a bit until both transition
    wait_next_state!(order_to_take.id, maker, taker, CfdState::PendingOpen);

    confirm!(lock transaction, order_to_take.id, maker, taker);
    wait_next_state!(order_to_take.id, maker, taker, CfdState::Open);

    (maker, taker, order_to_take.id)
}
