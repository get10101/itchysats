use anyhow::Context;
use anyhow::Result;
use daemon::projection::Cfd;
use daemon::projection::CfdOrder;
use daemon::projection::CfdState;
use daemon::tokio_ext::FutureExt;
use std::time::Duration;
use tokio::sync::watch;

/// Waiting time for the time on the watch channel before returning error
const NEXT_WAIT_TIME: Duration = Duration::from_secs(if cfg!(debug_assertions) { 180 } else { 30 });

pub async fn next_order(
    rx_a: &mut watch::Receiver<Option<CfdOrder>>,
    rx_b: &mut watch::Receiver<Option<CfdOrder>>,
) -> Result<(CfdOrder, CfdOrder)> {
    let wait_until_a = next_with(rx_a, |order| order);
    let wait_until_b = next_with(rx_b, |order| order);

    let (a, b) = tokio::join!(wait_until_a, wait_until_b);

    Ok((a?, b?))
}

/// Returns true if the next Option received on the stream is None
///
/// Returns false if Some is received.
pub async fn is_next_none<T>(rx: &mut watch::Receiver<Option<T>>) -> Result<bool>
where
    T: Clone,
{
    Ok(next(rx).await?.is_none())
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

    let val = wait_until_predicate
        .timeout(NEXT_WAIT_TIME)
        .await
        .with_context(|| {
            format!(
                "Value channel did not satisfy predicate within {} seconds",
                NEXT_WAIT_TIME.as_secs()
            )
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
