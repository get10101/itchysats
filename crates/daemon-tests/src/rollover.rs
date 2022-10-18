use crate::flow::next_with;
use crate::flow::one_cfd_with_state;
use crate::maia::OliviaData;
use crate::mock_oracle_announcements;
use crate::wait_next_state;
use crate::Maker;
use crate::Taker;
use daemon::bdk::bitcoin::SignedAmount;
use daemon::bdk::bitcoin::Txid;
use daemon::projection::CfdState;
use model::olivia::BitMexPriceEventId;
use model::OrderId;

pub async fn rollover(
    maker: &mut Maker,
    taker: &mut Taker,
    order_id: OrderId,
    oracle_data: OliviaData,
) {
    do_rollover(maker, taker, order_id, oracle_data, None).await
}

pub async fn rollover_from_commit_tx(
    maker: &mut Maker,
    taker: &mut Taker,
    order_id: OrderId,
    oracle_data: OliviaData,
    from_params_taker: Option<(Txid, BitMexPriceEventId)>,
) {
    do_rollover(maker, taker, order_id, oracle_data, from_params_taker).await
}

async fn do_rollover(
    maker: &mut Maker,
    taker: &mut Taker,
    order_id: OrderId,
    oracle_data: OliviaData,
    from_params_taker: Option<(Txid, BitMexPriceEventId)>,
) {
    // make sure the expected oracle data is mocked
    mock_oracle_announcements(maker, taker, oracle_data.announcements()).await;

    let commit_tx_id_before_rollover_maker = maker.latest_commit_txid();
    let commit_tx_id_before_rollover_taker = taker.latest_commit_txid();

    match from_params_taker {
        None => {
            taker
                .trigger_rollover_with_latest_dlc_params(order_id)
                .await;
        }
        Some((from_commit_txid, from_settlement_event_id)) => {
            taker
                .trigger_rollover_with_specific_params(
                    order_id,
                    from_commit_txid,
                    from_settlement_event_id,
                )
                .await;
        }
    }

    wait_next_state!(order_id, maker, taker, CfdState::RolloverSetup);
    wait_next_state!(order_id, maker, taker, CfdState::Open);

    assert_ne!(
        commit_tx_id_before_rollover_maker,
        maker.latest_commit_txid(),
        "The commit_txid of the taker should have changed after the rollover"
    );

    assert_ne!(
        commit_tx_id_before_rollover_taker,
        taker.latest_commit_txid(),
        "The commit_txid of the maker should have changed after the rollover"
    );

    assert_eq!(
        taker.latest_commit_txid(),
        maker.latest_commit_txid(),
        "The maker and taker should have the same commit_txid after the rollover"
    );

    // Ensure that the event ID of the latest dlc is the event ID used for rollover
    assert_eq!(
        oracle_data.settlement_announcement().id,
        maker.latest_settlement_event_id(),
        "Taker's latest event-id does not match given event-id after rollover"
    );
    assert_eq!(
        oracle_data.settlement_announcement().id,
        taker.latest_settlement_event_id(),
        "Taker's latest event-id does not match given event-id after rollover"
    );

    assert_eq!(
        taker.latest_revoked_revocation_sk_theirs(),
        Some(maker.latest_revoked_revocation_sk_ours()),
        "Taker receives maker's revocation sk during rollover"
    )
}

pub fn assert_rollover_fees(
    maker: &mut Maker,
    taker: &mut Taker,
    (expected_fees_after_rollover_maker, expected_fees_after_rollover_taker): (
        SignedAmount,
        SignedAmount,
    ),
) {
    assert_eq!(
        expected_fees_after_rollover_maker,
        maker.latest_accumulated_fees(),
        "Maker's fees don't match predicted fees after rollover"
    );
    assert_eq!(
        expected_fees_after_rollover_taker,
        taker.latest_accumulated_fees(),
        "Taker's fees don't match predicted fees after rollover"
    );
}
