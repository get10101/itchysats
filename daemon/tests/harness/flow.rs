use std::time::Duration;

use anyhow::Context;
use daemon::model::cfd::{Cfd, Order};
use daemon::tokio_ext::FutureExt;
use tokio::sync::watch;

/// Returns the first `Cfd` from both channels
///
/// Ensures that there is only one `Cfd` present in both channels.
pub async fn next_cfd(
    rx_a: &mut watch::Receiver<Vec<Cfd>>,
    rx_b: &mut watch::Receiver<Vec<Cfd>>,
) -> (Cfd, Cfd) {
    let (a, b) = tokio::join!(next(rx_a), next(rx_b));

    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);

    (a.first().unwrap().clone(), b.first().unwrap().clone())
}

pub async fn next_order(
    rx_a: &mut watch::Receiver<Option<Order>>,
    rx_b: &mut watch::Receiver<Option<Order>>,
) -> (Order, Order) {
    let (a, b) = tokio::join!(next_some(rx_a), next_some(rx_b));

    (a, b)
}

/// Returns the value if the next Option received on the stream is Some
///
/// Panics if None is received on the stream.
pub async fn next_some<T>(rx: &mut watch::Receiver<Option<T>>) -> T
where
    T: Clone,
{
    if let Some(value) = next(rx).await {
        value
    } else {
        panic!("Received None when Some was expected")
    }
}

/// Returns true if the next Option received on the stream is None
///
/// Returns false if Some is received.
pub async fn is_next_none<T>(rx: &mut watch::Receiver<Option<T>>) -> bool
where
    T: Clone,
{
    next(rx).await.is_none()
}

/// Returns watch channel value upon change
pub async fn next<T>(rx: &mut watch::Receiver<T>) -> T
where
    T: Clone,
{
    // TODO: Make timeout configurable, only contract setup can take up to 2 min on CI
    rx.changed()
        .timeout(Duration::from_secs(120))
        .await
        .context("Waiting for next element in channel is taking too long, aborting")
        .unwrap()
        .unwrap();
    rx.borrow().clone()
}
