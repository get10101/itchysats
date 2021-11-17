use futures::future::RemoteHandle;
use futures::FutureExt as _;
use std::fmt;
use std::future::Future;
use std::time::Duration;
use tokio::time::{timeout, Timeout};

pub fn spawn_fallible<F, E>(future: F)
where
    F: Future<Output = Result<(), E>> + Send + 'static,
    E: fmt::Display,
{
    // we want to disallow calls to tokio::spawn outside FutureExt
    #[allow(clippy::disallowed_method)]
    tokio::spawn(async move {
        if let Err(e) = future.await {
            tracing::warn!("Task failed: {:#}", e);
        }
    });
}

pub trait FutureExt: Future + Sized {
    fn timeout(self, duration: Duration) -> Timeout<Self>;

    /// Spawn the future on a task in the runtime and return a RemoteHandle to it.
    /// The task will be stopped when the handle gets dropped.
    fn spawn_with_handle(self) -> RemoteHandle<Self::Output>
    where
        Self: Future<Output = ()> + Send + 'static;
}

impl<F> FutureExt for F
where
    F: Future,
{
    fn timeout(self, duration: Duration) -> Timeout<F> {
        timeout(duration, self)
    }

    fn spawn_with_handle(self) -> RemoteHandle<()>
    where
        Self: Future<Output = ()> + Send + 'static,
    {
        let (future, handle) = self.remote_handle();
        // we want to disallow calls to tokio::spawn outside FutureExt
        #[allow(clippy::disallowed_method)]
        tokio::spawn(future);
        handle
    }
}
