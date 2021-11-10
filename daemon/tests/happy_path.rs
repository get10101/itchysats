use crate::harness::flow::{is_next_none, next_cfd, next_order, next_some};
use crate::harness::mocks::{
    mock_oracle_annoucement, mock_party_params, mock_wallet_sign_and_broadcast,
};
use crate::harness::start_both;
use daemon::maker_cfd;
use daemon::model::cfd::{CfdState, Order, Origin};
use daemon::model::{Price, Usd};
use maia::secp256k1_zkp::schnorrsig;
use rust_decimal_macros::dec;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
mod harness;

#[tokio::test]
async fn taker_receives_order_from_maker_on_publication() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    assert!(is_next_none(&mut taker.order_feed).await);

    maker.publish_order(dummy_new_order()).await;

    let (published, received) = tokio::join!(
        next_some(&mut maker.order_feed),
        next_some(&mut taker.order_feed)
    );

    assert_is_same_order(&published, &received);
}

#[tokio::test]
async fn taker_takes_order_and_maker_rejects() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    // TODO: Why is this needed? For the cfd stream it is not needed
    is_next_none(&mut taker.order_feed).await;

    maker.publish_order(dummy_new_order()).await;

    let (_, received) = next_order(&mut maker.order_feed, &mut taker.order_feed).await;

    taker.take_order(received.clone(), Usd::new(dec!(10)));

    let (taker_cfd, maker_cfd) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;
    assert_is_same_order(&taker_cfd.order, &received);
    assert_is_same_order(&maker_cfd.order, &received);
    assert!(matches!(
        taker_cfd.state,
        CfdState::OutgoingOrderRequest { .. }
    ));
    assert!(matches!(
        maker_cfd.state,
        CfdState::IncomingOrderRequest { .. }
    ));

    maker.reject_take_request(received.clone());

    let (taker_cfd, maker_cfd) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;
    // TODO: More elaborate Cfd assertions
    assert_is_same_order(&taker_cfd.order, &received);
    assert_is_same_order(&maker_cfd.order, &received);
    assert!(matches!(taker_cfd.state, CfdState::Rejected { .. }));
    assert!(matches!(maker_cfd.state, CfdState::Rejected { .. }));
}
#[tokio::test]
#[cfg_attr(not(feature = "expensive_tests"), ignore)]
async fn taker_takes_order_and_maker_accepts_and_contract_setup() {
    let _guard = init_tracing();
    let (mut maker, mut taker) = start_both().await;

    is_next_none(&mut taker.order_feed).await;

    maker.publish_order(dummy_new_order()).await;

    let (_, received) = next_order(&mut maker.order_feed, &mut taker.order_feed).await;

    taker.take_order(received.clone(), Usd::new(dec!(5)));
    let (_, _) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;

    maker.mocks.mock_oracle_annoucement().await;
    taker.mocks.mock_oracle_annoucement().await;

    maker.accept_take_request(received.clone());

    maker.mocks.mock_party_params().await;
    taker.mocks.mock_party_params().await;

    let (taker_cfd, maker_cfd) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;
    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::ContractSetup { .. }));
    assert!(matches!(maker_cfd.state, CfdState::ContractSetup { .. }));

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    let (taker_cfd, maker_cfd) = next_cfd(&mut taker.cfd_feed, &mut maker.cfd_feed).await;
    // TODO: More elaborate Cfd assertions
    assert_eq!(taker_cfd.order.id, received.id);
    assert_eq!(maker_cfd.order.id, received.id);
    assert!(matches!(taker_cfd.state, CfdState::PendingOpen { .. }));
    assert!(matches!(maker_cfd.state, CfdState::PendingOpen { .. }));
}

/// The order cannot be directly compared in tests as the origin is different,
/// therefore wrap the assertion macro in a code that unifies the 'Origin'
fn assert_is_same_order(a: &Order, b: &Order) {
    // Assume the same origin
    let mut a = a.clone();
    let mut b = b.clone();
    a.origin = Origin::Ours;
    b.origin = Origin::Ours;

    assert_eq!(a, b);
}

fn dummy_new_order() -> maker_cfd::NewOrder {
    maker_cfd::NewOrder {
        price: Price::new(dec!(50_000)).expect("unexpected failure"),
        min_quantity: Usd::new(dec!(5)),
        max_quantity: Usd::new(dec!(100)),
    }
}

fn init_tracing() -> DefaultGuard {
    let filter = EnvFilter::from_default_env()
        // apply warning level globally
        .add_directive(format!("{}", LevelFilter::WARN).parse().unwrap())
        // log traces from test itself
        .add_directive(
            format!("happy_path={}", LevelFilter::DEBUG)
                .parse()
                .unwrap(),
        )
        .add_directive(format!("taker={}", LevelFilter::DEBUG).parse().unwrap())
        .add_directive(format!("maker={}", LevelFilter::DEBUG).parse().unwrap())
        .add_directive(format!("daemon={}", LevelFilter::DEBUG).parse().unwrap())
        .add_directive(format!("rocket={}", LevelFilter::WARN).parse().unwrap());

    let guard = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_test_writer()
        .set_default();

    tracing::info!("Running version: {}", env!("VERGEN_GIT_SEMVER_LIGHTWEIGHT"));

    guard
}
