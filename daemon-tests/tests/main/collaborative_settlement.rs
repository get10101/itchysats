use daemon::projection::CfdState;
use daemon_tests::confirm;
use daemon_tests::flow::next_with;
use daemon_tests::flow::one_cfd_with_state;
use daemon_tests::maia::olivia::btc_example_0;
use daemon_tests::maia::olivia::eth_example_0;
use daemon_tests::mock_quotes;
use daemon_tests::open_cfd;
use daemon_tests::start_both;
use daemon_tests::wait_next_state;
use daemon_tests::OpenCfdArgs;
use model::ContractSymbol;
use model::Position;
use otel_tests::otel_test;

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
    let order_id = open_cfd(&mut taker, &mut maker, OpenCfdArgs::default()).await;
    mock_quotes(&mut maker, &mut taker).await;
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
    let order_id = open_cfd(&mut taker, &mut maker, OpenCfdArgs::default()).await;
    mock_quotes(&mut maker, &mut taker).await;

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
            ..Default::default()
        },
    )
    .await;
    mock_quotes(&mut maker, &mut taker).await;

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
}
