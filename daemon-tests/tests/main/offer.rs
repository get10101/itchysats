use daemon::bdk::bitcoin::Amount;
use daemon::projection::CfdOffer;
use daemon::projection::MakerOffers;
use daemon_tests::flow::is_next_offers_none;
use daemon_tests::flow::next_maker_offers;
use daemon_tests::start_both;
use daemon_tests::OfferParamsBuilder;
use model::ContractSymbol;
use model::Leverage;
use otel_tests::otel_test;

#[otel_test]
async fn taker_receives_offer_from_maker_on_publication() {
    let (mut maker, mut taker) = start_both().await;

    assert!(is_next_offers_none(taker.offers_feed()).await.unwrap());
    maker
        .set_offer_params(OfferParamsBuilder::new().build())
        .await;

    let (published, received) = next_maker_offers(
        maker.offers_feed(),
        taker.offers_feed(),
        &ContractSymbol::BtcUsd,
    )
    .await
    .unwrap();

    assert_eq_offers(published, received);
}

fn assert_eq_offers(published: MakerOffers, received: MakerOffers) {
    match (published.long, received.long) {
        (None, None) => (),
        (Some(published), Some(received)) => assert_eq_offer(published, received),
        (None, Some(_)) => panic!("Long offers did not match: Published None, received Some "),
        (Some(_), None) => panic!("Long offers did not match: Published Some, received None "),
    }

    match (published.short, received.short) {
        (None, None) => (),
        (Some(published), Some(received)) => assert_eq_offer(published, received),
        (None, Some(_)) => panic!("Short offers did not match: Published None, received Some "),
        (Some(_), None) => panic!("Short offers did not match: Published Some, received None "),
    }
}

fn assert_eq_offer(mut published: CfdOffer, received: CfdOffer) {
    // we fix the leverage to TWO for our test
    let fixed_leverage = Leverage::TWO;

    // make sure that the initial funding fee per lot is flipped
    // note: we publish as maker and receive as taker, the funding fee is to be received by one
    // party and paid by the other

    assert_eq!(
        published
            .leverage_details
            .iter()
            .find(|l| l.leverage == fixed_leverage)
            .unwrap()
            .initial_funding_fee_per_lot,
        received
            .leverage_details
            .iter()
            .find(|l| l.leverage == fixed_leverage)
            .unwrap()
            .initial_funding_fee_per_lot
            * -1
    );
    // align leverage details so we can assert on the offer
    published.leverage_details = received.leverage_details.clone();

    assert_eq!(published, received);

    // Hard-coded to match the dummy_new_offer()
    assert_eq!(received.opening_fee.unwrap(), Amount::from_sat(2));
    assert_eq!(received.funding_rate_hourly_percent, "0.00100");
}
