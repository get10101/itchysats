use anyhow::{Context, Result};
use daemon::model::cfd::{Cfd, Order};
use daemon::tokio_ext::FutureExt;
use std::time::Duration;
use tokio::sync::watch;

/// Returns the first `Cfd` from both channels
///
/// Ensures that there is only one `Cfd` present in both channels.
pub async fn next_cfd(
    rx_a: &mut watch::Receiver<Vec<Cfd>>,
    rx_b: &mut watch::Receiver<Vec<Cfd>>,
) -> Result<(Cfd, Cfd)> {
    let (a, b) = tokio::join!(next(rx_a), next(rx_b));
    let (a, b) = (a?, b?);

    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);

    Ok((a.first().unwrap().clone(), b.first().unwrap().clone()))
}

pub async fn next_order(
    rx_a: &mut watch::Receiver<Option<Order>>,
    rx_b: &mut watch::Receiver<Option<Order>>,
) -> Result<(Order, Order)> {
    let (a, b) = tokio::join!(next_some(rx_a), next_some(rx_b));

    Ok((a?, b?))
}

/// Returns the value if the next Option received on the stream is Some
pub async fn next_some<T>(rx: &mut watch::Receiver<Option<T>>) -> Result<T>
where
    T: Clone,
{
    next(rx)
        .await?
        .context("Received None when Some was expected")
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
    rx.changed()
        .timeout(Duration::from_secs(10))
        .await
        .context("No change in channel within 10 seconds")??;

    Ok(rx.borrow().clone())
}
