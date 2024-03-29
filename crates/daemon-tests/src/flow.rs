use anyhow::ensure;
use anyhow::Context;
use anyhow::Result;
use daemon::projection::Cfd;
use daemon::projection::CfdState;
use daemon::projection::MakerOffers;
use model::ContractSymbol;
use model::OrderId;
use std::time::Duration;
use tokio::sync::watch;

/// Waiting time for the time on the watch channel before returning error
const NEXT_WAIT_TIME: Duration = Duration::from_secs(if cfg!(debug_assertions) { 120 } else { 30 });

/// Wait and return next non-empty maker offers with `contract_symbol`
pub async fn next_maker_offers(
    rx_a: &mut watch::Receiver<MakerOffers>,
    rx_b: &mut watch::Receiver<MakerOffers>,
    contract_symbol: &ContractSymbol,
) -> Result<(MakerOffers, MakerOffers)> {
    let mut rx_a = rx_a.clone();
    let mut rx_b = rx_b.clone();

    let non_empty_offer = |offers: MakerOffers| match &offers {
        MakerOffers {
            btcusd_long: Some(_),
            ..
        } if contract_symbol == &ContractSymbol::BtcUsd => Some(offers),
        MakerOffers {
            btcusd_short: Some(_),
            ..
        } if contract_symbol == &ContractSymbol::BtcUsd => Some(offers),
        MakerOffers {
            ethusd_long: Some(_),
            ..
        } if contract_symbol == &ContractSymbol::EthUsd => Some(offers),
        MakerOffers {
            ethusd_short: Some(_),
            ..
        } if contract_symbol == &ContractSymbol::EthUsd => Some(offers),
        _ => None,
    };

    let wait_until_a = next_with(&mut rx_a, non_empty_offer);
    let wait_until_b = next_with(&mut rx_b, non_empty_offer);

    let (a, b) = tokio::join!(wait_until_a, wait_until_b);

    Ok((a?, b?))
}

pub async fn ensure_null_next_offers(rx: &mut watch::Receiver<MakerOffers>) -> Result<()> {
    let maker_offers = next(rx).await?;

    ensure!(
        maker_offers.btcusd_long.is_none()
            && maker_offers.btcusd_short.is_none()
            && maker_offers.ethusd_long.is_none()
            && maker_offers.ethusd_short.is_none()
    );

    Ok(())
}

/// Returns watch channel value upon change
pub async fn next<T>(rx: &mut watch::Receiver<T>) -> Result<T>
where
    T: Clone,
{
    next_with(rx, |v| Some(v)).await
}

/// Returns watch channel value upon change
pub async fn next_with<T, U>(
    rx: &mut watch::Receiver<T>,
    filter_map: impl Fn(T) -> Option<U>,
) -> Result<U>
where
    T: Clone,
{
    let wait_until_predicate = async {
        loop {
            rx.changed().await?;

            let current = rx.borrow().clone();

            if let Some(val) = filter_map(current) {
                return anyhow::Ok(val);
            }
        }
    };

    let val = tokio_extras::time::timeout(NEXT_WAIT_TIME, wait_until_predicate, || {
        tracing::debug_span!("wait until predicate")
    })
    .await
    .with_context(|| {
        let seconds = NEXT_WAIT_TIME.as_secs();

        format!("Value channel did not satisfy predicate within {seconds} seconds")
    })??;

    Ok(val)
}

/// Drop-in filter-map function for [`next_with`] to check the state of the CFD in a list of CFDs.
///
/// # Panics
///
/// If there is more than one CFD in the list. This is unsupported and unexpected by our test
/// framework.
pub fn one_cfd_with_state(expected_state: CfdState) -> impl Fn(Vec<Cfd>) -> Option<Cfd> {
    move |cfds: Vec<Cfd>| match cfds.as_slice() {
        [one] if one.state == expected_state => Some(one.clone()),
        [_one_that_doesnt_match_state] => None,
        [] => None,
        _more_than_one => panic!("More than one CFD in feed!"),
    }
}

pub fn cfd_with_state(
    order_id: OrderId,
    expected_state: CfdState,
) -> impl Fn(Vec<Cfd>) -> Option<Cfd> {
    move |cfds: Vec<Cfd>| match cfds.iter().find(|cfd| cfd.order_id == order_id) {
        Some(cfd) if cfd.state == expected_state => Some(cfd.clone()),
        Some(_cfd_that_does_not_match_state) => None,
        None => None,
    }
}
