use daemon::projection::CfdState;
use daemon_tests::confirm;
use daemon_tests::expire;
use daemon_tests::flow::next_with;
use daemon_tests::flow::one_cfd_with_state;
use daemon_tests::maia::OliviaData;
use daemon_tests::mock_oracle_announcements;
use daemon_tests::open_cfd;
use daemon_tests::simulate_attestation;
use daemon_tests::start_both;
use daemon_tests::wait_next_state;
use daemon_tests::OpenCfdArgs;
use model::Dlc;
use model::Price;
use otel_tests::otel_test;
use rust_decimal::Decimal;

#[otel_test]
async fn given_open_cfd_when_oracle_attests_long_liquidation_price_can_liquidate() {
    let (mut maker, mut taker) = start_both().await;

    let oracle_data = OliviaData::example_0();

    // We set the initial price to a value much higher than that of the price that the mock oracle
    // will attest to. This is so that the price attested to falls within the long liquidation
    // interval of the payout curve (the very first interval)
    let future_attestation_price = oracle_data.attested_price();
    let initial_price =
        Price::new(Decimal::from(future_attestation_price * 8)).expect("positive price");

    let order_id = open_cfd(
        &mut taker,
        &mut maker,
        OpenCfdArgs {
            initial_price,
            oracle_data: oracle_data.clone(),
            ..Default::default()
        },
    )
    .await;

    assert!(
        is_attestation_price_in_any_interval_of_all_liquidation_events(
            &taker.latest_dlc(),
            future_attestation_price
        )
    );

    taker.system.commit(order_id).await.unwrap();
    confirm!(commit transaction, order_id, maker, taker);
    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);

    expire!(cet timelock, order_id, maker, taker);

    let first_liquidation_event = taker.latest_dlc().liquidation_event_ids()[0];
    let attestation = oracle_data
        .attestation_for_event(first_liquidation_event)
        .unwrap();

    simulate_attestation!(taker, maker, order_id, &attestation);
    wait_next_state!(order_id, maker, taker, CfdState::PendingCet);

    confirm!(cet, order_id, maker, taker);
    wait_next_state!(order_id, maker, taker, CfdState::Closed);
}

#[otel_test]
async fn given_rollover_when_oracle_attests_long_liquidation_price_can_liquidate() {
    let (mut maker, mut taker) = start_both().await;

    let oracle_data_contract_setup = OliviaData::example_0();

    // We set the initial price to a value much higher than that of the price that the mock oracle
    // will attest to. This is so that the price attested to falls within the long liquidation
    // interval of the payout curve (the very first interval)
    let future_attestation_price = oracle_data_contract_setup.attested_price();
    let initial_price =
        Price::new(Decimal::from(future_attestation_price * 8)).expect("positive price");

    let order_id = open_cfd(
        &mut taker,
        &mut maker,
        OpenCfdArgs {
            initial_price,
            oracle_data: oracle_data_contract_setup,
            ..Default::default()
        },
    )
    .await;

    assert!(
        is_attestation_price_in_any_interval_of_all_liquidation_events(
            &taker.latest_dlc(),
            future_attestation_price
        )
    );

    let oracle_data_rollover = OliviaData::example_1();
    mock_oracle_announcements(&mut maker, &mut taker, oracle_data_rollover.announcements()).await;

    taker
        .trigger_rollover_with_latest_dlc_params(order_id)
        .await;

    taker.system.commit(order_id).await.unwrap();
    confirm!(commit transaction, order_id, maker, taker);
    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);

    expire!(cet timelock, order_id, maker, taker);

    let first_liquidation_event = taker.latest_dlc().liquidation_event_ids()[0];
    let attestation = oracle_data_rollover
        .attestation_for_event(first_liquidation_event)
        .unwrap();

    simulate_attestation!(taker, maker, order_id, &attestation);
    wait_next_state!(order_id, maker, taker, CfdState::PendingCet);

    confirm!(cet, order_id, maker, taker);
    wait_next_state!(order_id, maker, taker, CfdState::Closed);
}

/// Verify that the mocked attestation price plays nicely with the mocked initial price of the CFD.
///
/// Checking this ensures that all liquidation events are "active" after we mock the oracle
/// attestations. By active we mean that at least one CET per liquidation event ID will be
/// successfully decrypted in combination with the oracle attestation.
fn is_attestation_price_in_any_interval_of_all_liquidation_events(
    dlc: &Dlc,
    attestation_price: u64,
) -> bool {
    let settlement_event_id = dlc.settlement_event_id;

    let cets_per_event_id = &dlc.cets;
    let mut liquidation_cets_per_event_id = cets_per_event_id
        .iter()
        .filter(|(id, _)| **id != settlement_event_id);

    liquidation_cets_per_event_id.all(|(_, cets)| {
        cets.iter()
            .any(|cet| cet.range.contains(&attestation_price))
    })
}
