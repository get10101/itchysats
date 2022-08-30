use daemon::projection::CfdState;
use daemon_tests::confirm;
use daemon_tests::dummy_btc_price;
use daemon_tests::dummy_eth_price;
use daemon_tests::expected_maker_liquidation_price;
use daemon_tests::expected_taker_liquidation_price;
use daemon_tests::flow::next_with;
use daemon_tests::flow::one_cfd_with_state;
use daemon_tests::initial_price_for;
use daemon_tests::maia::olivia::btc_example_0;
use daemon_tests::maia::olivia::eth_example_0;
use daemon_tests::mock_quotes;
use daemon_tests::open_cfd;
use daemon_tests::start_both;
use daemon_tests::wait_next_state;
use daemon_tests::Maker;
use daemon_tests::OpenCfdArgs;
use daemon_tests::Taker;
use model::ContractSymbol;
use model::Position;
use otel_tests::otel_test;
use rust_decimal_macros::dec;

#[otel_test]
async fn collaboratively_close_an_open_btc_usd_cfd_maker_going_short() {
    collaboratively_close_an_open_cfd(Position::Short, ContractSymbol::BtcUsd).await;
}

#[otel_test]
async fn collaboratively_close_an_open_btc_usd_cfd_maker_going_long() {
    collaboratively_close_an_open_cfd(Position::Long, ContractSymbol::BtcUsd).await;
}

#[otel_test]
async fn collaboratively_close_an_open_eth_usd_cfd_maker_going_short() {
    collaboratively_close_an_open_cfd(Position::Short, ContractSymbol::EthUsd).await;
}

#[otel_test]
async fn collaboratively_close_an_open_eth_usd_cfd_maker_going_long() {
    collaboratively_close_an_open_cfd(Position::Long, ContractSymbol::EthUsd).await;
}

#[otel_test]
async fn maker_rejects_collab_settlement_after_commit_finality() {
    let (mut maker, mut taker) = start_both().await;
    let cfd_args = OpenCfdArgs::default();
    let order_id = open_cfd(&mut taker, &mut maker, cfd_args.clone()).await;
    mock_quotes(&mut maker, &mut taker, cfd_args.contract_symbol).await;
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
    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);
}

#[otel_test]
async fn maker_accepts_collab_settlement_after_commit_finality() {
    let (mut maker, mut taker) = start_both().await;
    let cfd_args = OpenCfdArgs::default();
    let order_id = open_cfd(&mut taker, &mut maker, cfd_args.clone()).await;
    mock_quotes(&mut maker, &mut taker, cfd_args.contract_symbol).await;

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
    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);
}

async fn collaboratively_close_an_open_cfd(
    position_maker: Position,
    contract_symbol: ContractSymbol,
) {
    let (mut maker, mut taker) = start_both().await;
    let oracle_data = match contract_symbol {
        ContractSymbol::BtcUsd => btc_example_0(),
        ContractSymbol::EthUsd => eth_example_0(),
    };
    let order_id = open_cfd(
        &mut taker,
        &mut maker,
        OpenCfdArgs {
            position_maker,
            contract_symbol,
            oracle_data,
            initial_price: initial_price_for(contract_symbol),
            ..Default::default()
        },
    )
    .await;
    mock_quotes(&mut maker, &mut taker, contract_symbol).await;

    taker.system.propose_settlement(order_id).await.unwrap();

    wait_next_state!(
        order_id,
        maker,
        taker,
        CfdState::IncomingSettlementProposal,
        CfdState::OutgoingSettlementProposal
    );

    maker.system.accept_settlement(order_id).await.unwrap();
    wait_next_state!(order_id, maker, taker, CfdState::PendingClose);

    confirm!(close transaction, order_id, maker, taker);
    wait_next_state!(order_id, maker, taker, CfdState::Closed);
    verify_closed_cfds(&mut maker, &mut taker);
}

/// Sanity check the values we publish in projection
fn verify_closed_cfds(maker: &mut Maker, taker: &mut Taker) {
    assert_eq!(
        maker.first_cfd().closing_price.unwrap(),
        taker.first_cfd().closing_price.unwrap()
    );
    assert_eq!(
        maker.first_cfd().contract_symbol,
        taker.first_cfd().contract_symbol
    );

    assert_ne!(maker.first_cfd().position, taker.first_cfd().position);

    assert_eq!(
        maker.first_cfd().closing_price.unwrap(),
        initial_price_for(maker.first_cfd().contract_symbol),
        "We use same dummy price for offers and quotes everywhere"
    );

    assert_maker_liquidation_price(maker);
    assert_taker_liquidation_price(taker);
}

fn assert_maker_liquidation_price(maker: &mut Maker) {
    assert_eq!(dummy_btc_price(), dec!(50_000), "precondition");
    assert_eq!(dummy_eth_price(), dec!(1_500), "precondition");

    let maker_position = maker.first_cfd().position;
    let symbol = maker.first_cfd().contract_symbol;
    assert_eq!(
        maker.first_cfd().liquidation_price,
        expected_maker_liquidation_price(symbol, maker_position)
    );
}

fn assert_taker_liquidation_price(taker: &mut Taker) {
    assert_eq!(dummy_btc_price(), dec!(50_000), "precondition");
    assert_eq!(dummy_eth_price(), dec!(1_500), "precondition");

    let taker_position = taker.first_cfd().position;
    let symbol = taker.first_cfd().contract_symbol;
    assert_eq!(
        taker.first_cfd().liquidation_price,
        expected_taker_liquidation_price(symbol, taker_position)
    );
}
