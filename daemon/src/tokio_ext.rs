use std::fmt;
use std::future::Future;
use std::time::Duration;
use tokio::time::{timeout, Timeout};

pub fn spawn_fallible<F, E>(future: F)
where
    F: Future<Output = Result<(), E>> + Send + 'static,
    E: fmt::Display,
{
    tokio::spawn(async move {
        if let Err(e) = future.await {
            tracing::warn!("Task failed: {:#}", e);
        }
    });
}

pub trait FutureExt: Future + Sized {
    fn timeout(self, duration: Duration) -> Timeout<Self>;
}

impl<F> FutureExt for F
where
    F: Future,
{
    fn timeout(self, duration: Duration) -> Timeout<F> {
        timeout(duration, self)
    }
}
