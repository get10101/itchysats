use daemon::projection::CfdOffer;
use daemon::projection::MakerOffers;
use daemon_tests::flow::ensure_null_next_offers;
use daemon_tests::flow::next_maker_offers;
use daemon_tests::start_both;
use daemon_tests::Maker;
use daemon_tests::OfferParamsBuilder;
use daemon_tests::Taker;
use model::ContractSymbol;
use model::Leverage;
use otel_tests::otel_test;

#[otel_test]
async fn taker_receives_btc_usd_offer_from_maker_on_publication() {
    taker_receives_offer_from_maker_on_publication(ContractSymbol::BtcUsd).await;
}

#[otel_test]
async fn taker_receives_eth_usd_offer_from_maker_on_publication() {
    taker_receives_offer_from_maker_on_publication(ContractSymbol::EthUsd).await;
}

#[otel_test]
async fn taker_can_receive_offers_with_different_symbols_from_maker() {
    let (mut maker, mut taker) = start_both().await;
    ensure_null_next_offers(taker.offers_feed()).await.unwrap();

    test_offer(&mut maker, &mut taker, ContractSymbol::BtcUsd).await;
    test_offer(&mut maker, &mut taker, ContractSymbol::EthUsd).await;
}

async fn publish_offer(maker: &mut Maker, contract_symbol: ContractSymbol) {
    let leverage = Leverage::TWO;
    maker
        .set_offer_params(
            OfferParamsBuilder::new(contract_symbol)
                .leverage_choices(vec![leverage])
                .build(),
        )
        .await;
}

/// Verify that an offer with a given contract symbol gets published by the
/// maker an is received by the taker
async fn test_offer(maker: &mut Maker, taker: &mut Taker, symbol: ContractSymbol) {
    publish_offer(maker, symbol).await;
    let (published, received) =
        next_maker_offers(maker.offers_feed(), taker.offers_feed(), &symbol)
            .await
            .unwrap();
    assert_eq_offers(published, received);
}

async fn taker_receives_offer_from_maker_on_publication(contract_symbol: ContractSymbol) {
    let (mut maker, mut taker) = start_both().await;
    ensure_null_next_offers(taker.offers_feed()).await.unwrap();
    test_offer(&mut maker, &mut taker, contract_symbol).await;
}

fn assert_eq_offers(published: MakerOffers, received: MakerOffers) {
    assert_eq_offer(published.btcusd_long, received.btcusd_long);
    assert_eq_offer(published.btcusd_short, received.btcusd_short);
    assert_eq_offer(published.ethusd_long, received.ethusd_long);
    assert_eq_offer(published.ethusd_short, received.ethusd_short);
}

/// Helper function to compare a maker's `CfdOffer` against the taker's corresponding `CfdOffer`.
///
/// Unfortunately, we cannot simply use `assert_eq!` because part of the `CfdOffer` is
/// position-depedent.
fn assert_eq_offer(published: Option<CfdOffer>, received: Option<CfdOffer>) {
    let (mut published, mut received) = match (published, received) {
        (None, None) => return,
        (Some(published), Some(received)) => (published, received),
        (published, received) => {
            panic!("Offer mismatch. Maker published {published:?}, taker received {received:?}")
        }
    };

    // Comparing `LeverageDetails` straight up will fail because the values in them depend on each
    // party's position. Therefore, we need to assert against things carefully
    {
        for (leverage_details_published_i, leverage_details_received_i) in published
            .leverage_details
            .iter()
            .zip(received.leverage_details.iter())
        {
            // We can expect the absolute values of the initial funding fee per lot to be the same
            // per leverage for both parties
            let initial_funding_fee_per_lot_maker =
                leverage_details_published_i.initial_funding_fee_per_lot;
            let initial_funding_fee_per_lot_taker =
                leverage_details_received_i.initial_funding_fee_per_lot;
            assert_eq!(
                initial_funding_fee_per_lot_maker.abs(),
                initial_funding_fee_per_lot_taker.abs()
            );
        }

        // As a last step, we delete the data from `leverage_details` for both parties so that the
        // final assertion on the entire `CfdOffer` has a chance of succeeding
        published.leverage_details = Vec::new();
        received.leverage_details = Vec::new();
    }

    assert_eq!(published, received);
}
