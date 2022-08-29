use daemon::bdk::bitcoin::SignedAmount;
use daemon::bdk::bitcoin::Txid;
use daemon::projection::CfdState;
use daemon_tests::flow::next_with;
use daemon_tests::flow::one_cfd_with_state;
use daemon_tests::maia::olivia::btc_example_0;
use daemon_tests::maia::olivia::btc_example_1;
use daemon_tests::maia::olivia::eth_example_0;
use daemon_tests::maia::OliviaData;
use daemon_tests::open_cfd;
use daemon_tests::start_both;
use daemon_tests::wait_next_state;
use daemon_tests::FeeCalculator;
use daemon_tests::Maker;
use daemon_tests::OfferParamsBuilder;
use daemon_tests::OpenCfdArgs;
use daemon_tests::Taker;
use model::olivia::BitMexPriceEventId;
use model::ContractSymbol;
use model::OrderId;
use model::Position;
use otel_tests::otel_test;

#[otel_test]
async fn rollover_an_open_btc_usd_cfd_maker_going_short() {
    let (mut maker, mut taker, order_id, fee_calculator) =
        prepare_rollover(Position::Short, ContractSymbol::BtcUsd, btc_example_0()).await;

    rollover(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_0(),
        fee_calculator.complete_fee_for_expired_settlement_event(),
    )
    .await;
}

#[otel_test]
async fn rollover_an_open_eth_usd_cfd_maker_going_short() {
    let (mut maker, mut taker, order_id, fee_calculator) =
        prepare_rollover(Position::Short, ContractSymbol::EthUsd, eth_example_0()).await;

    rollover(
        &mut maker,
        &mut taker,
        order_id,
        eth_example_0(),
        fee_calculator.complete_fee_for_expired_settlement_event(),
    )
    .await;
}

#[otel_test]
async fn rollover_an_open_cfd_maker_going_long() {
    let (mut maker, mut taker, order_id, fee_calculator) =
        prepare_rollover(Position::Long, ContractSymbol::BtcUsd, btc_example_0()).await;

    rollover(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_0(),
        fee_calculator.complete_fee_for_expired_settlement_event(),
    )
    .await;
}

#[otel_test]
async fn double_rollover_an_open_cfd() {
    // double rollover ensures that both parties properly succeeded and can do another rollover

    let (mut maker, mut taker, order_id, fee_calculator) =
        prepare_rollover(Position::Short, ContractSymbol::BtcUsd, btc_example_0()).await;

    rollover(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_0(),
        fee_calculator.complete_fee_for_expired_settlement_event(),
    )
    .await;

    rollover(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_0(),
        fee_calculator.complete_fee_for_rollover_hours(48),
    )
    .await;
}

#[otel_test]
async fn maker_rejects_rollover_of_open_cfd() {
    let (mut maker, mut taker) = start_both().await;
    let order_id = open_cfd(&mut taker, &mut maker, OpenCfdArgs::default()).await;

    let taker_commit_txid_after_contract_setup = taker.latest_commit_txid();
    let maker_commit_txid_after_contract_setup = taker.latest_commit_txid();

    let is_accepting_rollovers = false;
    maker
        .system
        .update_rollover_configuration(is_accepting_rollovers)
        .await
        .unwrap();

    taker
        .trigger_rollover_with_latest_dlc_params(order_id)
        .await;

    wait_next_state!(order_id, maker, taker, CfdState::Open);

    let taker_commit_txid_after_rejected_rollover = taker.latest_commit_txid();
    assert_eq!(
        taker_commit_txid_after_contract_setup,
        taker_commit_txid_after_rejected_rollover
    );

    let maker_commit_txid_after_rejected_rollover = taker.latest_commit_txid();
    assert_eq!(
        maker_commit_txid_after_contract_setup,
        maker_commit_txid_after_rejected_rollover
    );
}

#[otel_test]
async fn given_rollover_completed_when_taker_fails_rollover_can_retry() {
    let (mut maker, mut taker, order_id, fee_calculator) =
        prepare_rollover(Position::Short, ContractSymbol::BtcUsd, btc_example_0()).await;

    // 1. Do two rollovers
    rollover(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_1(),
        fee_calculator.complete_fee_for_expired_settlement_event(),
    )
    .await;

    let taker_commit_txid_after_first_rollover = taker.latest_commit_txid();
    let taker_settlement_event_id_after_first_rollover = taker.latest_settlement_event_id();
    let taker_dlc_after_first_rollover = taker.latest_dlc();
    let taker_complete_fee_after_first_rollover = taker.latest_complete_fee();

    rollover(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_1(),
        // The second rollover increases the complete fees to 48h
        fee_calculator.complete_fee_for_rollover_hours(48),
    )
    .await;

    // We simulate the taker being one rollover behind by setting the
    // latest DLC to the one of the first rollover
    taker
        .append_rollover_event(
            order_id,
            taker_dlc_after_first_rollover,
            taker_complete_fee_after_first_rollover,
        )
        .await;

    // 2. Retry the rollover from the first rollover DLC
    rollover_from_commit_tx(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_1(),
        Some((
            taker_commit_txid_after_first_rollover,
            taker_settlement_event_id_after_first_rollover,
        )),
        // We expect that the rollover retry won't add additional costs, since we retry from the
        // previous rollover we expect 48h
        fee_calculator.complete_fee_for_rollover_hours(48),
    )
    .await;

    // 3. Ensure that we can do another rollover after the retry
    rollover(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_1(),
        // Additional rollover increases the complete fees to 72h
        fee_calculator.complete_fee_for_rollover_hours(72),
    )
    .await;
}

#[otel_test]
async fn given_contract_setup_completed_when_taker_fails_first_rollover_can_retry() {
    let (mut maker, mut taker, order_id, fee_calculator) =
        prepare_rollover(Position::Short, ContractSymbol::BtcUsd, btc_example_0()).await;

    let taker_commit_txid_after_contract_setup = taker.latest_commit_txid();
    let taker_settlement_event_id_after_contract_setup = taker.latest_settlement_event_id();
    let taker_dlc_after_contract_setup = taker.latest_dlc();
    let taker_complete_fee_after_contract_setup = taker.latest_complete_fee();

    // 1. Do a rollover
    // For the first rollover we expect to be charged 24h
    rollover(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_1(),
        fee_calculator.complete_fee_for_expired_settlement_event(),
    )
    .await;

    // 2. Retry the rollover
    // We simulate the taker being one rollover behind by setting the
    // latest DLC to the one generated by contract setup
    taker
        .append_rollover_event(
            order_id,
            taker_dlc_after_contract_setup,
            taker_complete_fee_after_contract_setup,
        )
        .await;

    // When retrying the rollover we expect to be charged the same amount
    rollover_from_commit_tx(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_1(),
        Some((
            taker_commit_txid_after_contract_setup,
            taker_settlement_event_id_after_contract_setup,
        )),
        fee_calculator.complete_fee_for_expired_settlement_event(),
    )
    .await;

    // 3. Ensure that we can do another rollover after the retry
    // After another rollover we expect to be charged for 48h
    rollover(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_1(),
        fee_calculator.complete_fee_for_rollover_hours(48),
    )
    .await;
}

#[otel_test]
async fn given_contract_setup_completed_when_taker_fails_two_rollovers_can_retry() {
    let (mut maker, mut taker, order_id, fee_calculator) =
        prepare_rollover(Position::Short, ContractSymbol::BtcUsd, btc_example_0()).await;

    let taker_commit_txid_after_contract_setup = taker.latest_commit_txid();
    let taker_settlement_event_id_after_contract_setup = taker.latest_settlement_event_id();
    let taker_dlc_after_contract_setup = taker.latest_dlc();
    let taker_complete_fee_after_contract_setup = taker.latest_complete_fee();

    // 1. Do two rollovers
    rollover(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_1(),
        fee_calculator.complete_fee_for_rollover_hours(24),
    )
    .await;

    rollover(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_1(),
        // The second rollover increases the complete fees to 48h
        fee_calculator.complete_fee_for_rollover_hours(48),
    )
    .await;

    // 2. Retry the rollover from contract setup, i.e. both rollovers are discarded, we go back to
    // the initial DLC state We simulate the taker being two rollover behind by setting the
    // latest DLC to the one generated by contract setup
    taker
        .append_rollover_event(
            order_id,
            taker_dlc_after_contract_setup,
            taker_complete_fee_after_contract_setup,
        )
        .await;

    rollover_from_commit_tx(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_1(),
        Some((
            taker_commit_txid_after_contract_setup,
            taker_settlement_event_id_after_contract_setup,
        )),
        fee_calculator.complete_fee_for_expired_settlement_event(),
    )
    .await;

    // 3. Ensure that we can do another rollover after the retry
    rollover(
        &mut maker,
        &mut taker,
        order_id,
        btc_example_1(),
        // After another rollover we expect to be charged for 48h
        fee_calculator.complete_fee_for_rollover_hours(48),
    )
    .await;
}

/// Set up a CFD that can be rolled over
///
/// Starts maker and taker with an open CFD.
/// Sets offer on the maker side because in order to roll over the maker needs an active offer for
/// up-to-date fee calculation.
/// Asserts that initially both parties don't have funding costs.
async fn prepare_rollover(
    position_maker: Position,
    contract_symbol: ContractSymbol,
    oracle_data: OliviaData,
) -> (Maker, Taker, OrderId, FeeCalculator) {
    let (mut maker, mut taker) = start_both().await;

    let open_cfd_args = OpenCfdArgs {
        position_maker,
        oracle_data,
        contract_symbol,
        ..Default::default()
    };
    let fee_calculator = open_cfd_args.fee_calculator();

    let order_id = open_cfd(&mut taker, &mut maker, open_cfd_args).await;

    // Maker needs to have an active offer in order to accept rollover
    maker
        .set_offer_params(OfferParamsBuilder::new(contract_symbol).build())
        .await;

    let maker_cfd = maker.first_cfd();
    let taker_cfd = taker.first_cfd();

    let (expected_maker_fee, expected_taker_fee) =
        fee_calculator.complete_fee_for_rollover_hours(0);
    assert_eq!(expected_maker_fee, maker_cfd.accumulated_fees);
    assert_eq!(expected_taker_fee, taker_cfd.accumulated_fees);

    (maker, taker, order_id, fee_calculator)
}

pub async fn rollover(
    maker: &mut Maker,
    taker: &mut Taker,
    order_id: OrderId,
    oracle_data: OliviaData,
    expected_fees: (SignedAmount, SignedAmount),
) {
    daemon_tests::rollover::rollover(maker, taker, order_id, oracle_data).await;
    daemon_tests::rollover::assert_rollover_fees(maker, taker, expected_fees);
}

pub async fn rollover_from_commit_tx(
    maker: &mut Maker,
    taker: &mut Taker,
    order_id: OrderId,
    oracle_data: OliviaData,
    from_params_taker: Option<(Txid, BitMexPriceEventId)>,
    expected_fees: (SignedAmount, SignedAmount),
) {
    daemon_tests::rollover::rollover_from_commit_tx(
        maker,
        taker,
        order_id,
        oracle_data,
        from_params_taker,
    )
    .await;
    daemon_tests::rollover::assert_rollover_fees(maker, taker, expected_fees);
}
