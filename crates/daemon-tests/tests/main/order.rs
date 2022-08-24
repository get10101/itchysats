use daemon::projection::CfdState;
use daemon_tests::confirm;
use daemon_tests::flow::cfd_with_state;
use daemon_tests::flow::ensure_null_next_offers;
use daemon_tests::flow::next_maker_offers;
use daemon_tests::flow::next_with;
use daemon_tests::flow::one_cfd_with_state;
use daemon_tests::start_both;
use daemon_tests::wait_next_state;
use daemon_tests::wait_next_state_multi_cfd;
use daemon_tests::Maker;
use daemon_tests::OfferParamsBuilder;
use daemon_tests::Taker;
use model::ContractSymbol;
use model::Contracts;
use model::Leverage;
use model::OrderId;
use otel_tests::otel_test;

#[otel_test]
async fn taker_places_order_and_maker_rejects() {
    let (mut maker, mut taker) = start_both().await;

    ensure_null_next_offers(taker.offers_feed()).await.unwrap();

    maker
        .set_offer_params(OfferParamsBuilder::new().build())
        .await;

    let (_, received) = next_maker_offers(
        maker.offers_feed(),
        taker.offers_feed(),
        &ContractSymbol::BtcUsd,
    )
    .await
    .unwrap();

    let offer_id = received.btcusd_short.unwrap().id;

    taker.mocks.mock_oracle_announcement().await;
    maker.mocks.mock_oracle_announcement().await;
    let order_id = taker
        .system
        .place_order(offer_id, Contracts::new(100), Leverage::TWO)
        .await
        .unwrap();

    wait_next_state!(order_id, maker, taker, CfdState::PendingSetup);

    maker.system.reject_order(order_id).await.unwrap();

    wait_next_state!(order_id, maker, taker, CfdState::Rejected);
}

#[otel_test]
async fn taker_places_order_and_maker_accepts_and_contract_setup() {
    let (mut maker, mut taker) = start_both().await;

    ensure_null_next_offers(taker.offers_feed()).await.unwrap();

    maker
        .set_offer_params(OfferParamsBuilder::new().build())
        .await;

    let (_, received) = next_maker_offers(
        maker.offers_feed(),
        taker.offers_feed(),
        &ContractSymbol::BtcUsd,
    )
    .await
    .unwrap();

    let offer_id = received.btcusd_short.unwrap().id;

    taker.mocks.mock_oracle_announcement().await;
    maker.mocks.mock_oracle_announcement().await;

    let order_id = taker
        .system
        .place_order(offer_id, Contracts::new(100), Leverage::TWO)
        .await
        .unwrap();

    first_contract_setup(&mut maker, &mut taker, order_id).await;
}

#[otel_test]
async fn taker_places_order_for_same_offer_twice_results_in_two_cfds() {
    let (mut maker, mut taker) = start_both().await;

    ensure_null_next_offers(taker.offers_feed()).await.unwrap();

    maker
        .set_offer_params(OfferParamsBuilder::new().build())
        .await;

    let (_, received) = next_maker_offers(
        maker.offers_feed(),
        taker.offers_feed(),
        &ContractSymbol::BtcUsd,
    )
    .await
    .unwrap();

    let offer_id = received.btcusd_short.unwrap().id;

    taker.mocks.mock_oracle_announcement().await;
    maker.mocks.mock_oracle_announcement().await;
    let first_order_id = taker
        .system
        .place_order(offer_id, Contracts::new(10), Leverage::TWO)
        .await
        .unwrap();

    first_contract_setup(&mut maker, &mut taker, first_order_id).await;

    let second_order_id = taker
        .system
        .place_order(offer_id, Contracts::new(10), Leverage::TWO)
        .await
        .unwrap();

    additional_contract_setup(&mut maker, &mut taker, second_order_id).await;

    let taker_cfds = taker.cfds();
    assert_eq!(
        taker_cfds.len(),
        2,
        "taker should have two Cfd after two orders were filled"
    );

    let first_cfd = taker_cfds
        .iter()
        .find(|cfd| cfd.order_id == first_order_id)
        .unwrap();
    let second_cfd = taker_cfds
        .iter()
        .find(|cfd| cfd.order_id == second_order_id)
        .unwrap();

    assert_eq!(
        first_order_id, first_cfd.order_id,
        "First taker cfd's order_id id does not match order_id"
    );
    assert_eq!(
        offer_id, first_cfd.offer_id,
        "First taker cfd's offer_id does not match offer_id"
    );
    assert_eq!(
        second_order_id, second_cfd.order_id,
        "Second taker cfd's order_id id does not match order_id"
    );
    assert_eq!(
        offer_id, second_cfd.offer_id,
        "Second taker cfd's offer_id does not match offer_id"
    );

    let maker_cfds = maker.cfds();
    assert_eq!(
        maker_cfds.len(),
        2,
        "maker should have two Cfd after two orders were filled"
    );

    let first_cfd = maker_cfds
        .iter()
        .find(|cfd| cfd.order_id == first_order_id)
        .unwrap();
    let second_cfd = maker_cfds
        .iter()
        .find(|cfd| cfd.order_id == second_order_id)
        .unwrap();

    assert_eq!(
        first_order_id, first_cfd.order_id,
        "First maker cfd's order_id id does not match order_id"
    );
    assert_eq!(
        offer_id, first_cfd.offer_id,
        "First maker cfd's offer_id does not match offer_id"
    );
    assert_eq!(
        second_order_id, second_cfd.order_id,
        "Second maker cfd's order_id id does not match order_id"
    );
    assert_eq!(
        offer_id, second_cfd.offer_id,
        "Second maker cfd's offer_id does not match offer_id"
    );
}

/// To be used for the first contract setup
async fn first_contract_setup(maker: &mut Maker, taker: &mut Taker, order_id: OrderId) {
    wait_next_state!(order_id, maker, taker, CfdState::PendingSetup);

    maker.mocks.mock_party_params().await;
    taker.mocks.mock_party_params().await;

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    maker.system.accept_order(order_id).await.unwrap();
    wait_next_state!(order_id, maker, taker, CfdState::ContractSetup);

    wait_next_state!(order_id, maker, taker, CfdState::PendingOpen);

    confirm!(lock transaction, order_id, maker, taker);
    wait_next_state!(order_id, maker, taker, CfdState::Open);
}

/// To be used for any additional contract setup
///
/// Note that we don't assert on the number of cfds, but just try to find the cfd with the given id.
async fn additional_contract_setup(maker: &mut Maker, taker: &mut Taker, order_id: OrderId) {
    wait_next_state_multi_cfd!(order_id, maker, taker, CfdState::PendingSetup);

    maker.mocks.mock_party_params().await;
    taker.mocks.mock_party_params().await;

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    maker.system.accept_order(order_id).await.unwrap();
    wait_next_state_multi_cfd!(order_id, maker, taker, CfdState::ContractSetup);

    wait_next_state_multi_cfd!(order_id, maker, taker, CfdState::PendingOpen);

    confirm!(lock transaction, order_id, maker, taker);
    wait_next_state_multi_cfd!(order_id, maker, taker, CfdState::Open);
}
