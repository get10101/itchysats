use futures::future::RemoteHandle;
use futures::FutureExt as _;
use std::any::{Any, TypeId};
use std::fmt;
use std::future::Future;
use std::time::Duration;
use tokio::time::{timeout, Timeout};

pub fn spawn_fallible<F, E>(future: F, name: &str)
where
    F: Future<Output = Result<(), E>> + Send + 'static,
    E: fmt::Display,
{
    // we want to disallow calls to tokio::spawn outside FutureExt
    #[allow(clippy::disallowed_method)]
    tokio::task::Builder::new().name(name).spawn(async move {
        if let Err(e) = future.await {
            tracing::warn!("Task failed: {:#}", e);
        }
    });
    // tokio::spawn(async move {
    //     if let Err(e) = future.await {
    //         tracing::warn!("Task failed: {:#}", e);
    //     }
    // });
}

pub trait FutureExt: Future + Sized {
    fn timeout(self, duration: Duration) -> Timeout<Self>;

    /// Spawn the future on a task in the runtime and return a RemoteHandle to it.
    /// The task will be stopped when the handle gets dropped.
    fn spawn_with_handle(self, x: &str) -> RemoteHandle<Self::Output>
    where
        Self: Future<Output = ()> + Send + Any + 'static;
}

impl<F> FutureExt for F
where
    F: Future,
{
    fn timeout(self, duration: Duration) -> Timeout<F> {
        timeout(duration, self)
    }

    fn spawn_with_handle(self, name: &str) -> RemoteHandle<()>
    where
        Self: Future<Output = ()> + Send + Any + 'static,
    {
        debug_assert!(
            TypeId::of::<RemoteHandle<()>>() != self.type_id(),
            "RemoteHandle<()> is a handle to already spawned task",
        );

        let (future, handle) = self.remote_handle();
        // we want to disallow calls to tokio::spawn outside FutureExt
        #[allow(clippy::disallowed_method)]
        tokio::task::Builder::new().name(name).spawn(future);
        // tokio::spawn(future);

        handle
    }
}

#[cfg(test)]
mod tests {
    use std::panic;

    use tokio::time::sleep;

    use super::*;

    #[tokio::test]
    async fn spawning_a_regular_future_does_not_panic() {
        let result = panic::catch_unwind(|| {
            let _handle = sleep(Duration::from_secs(2)).spawn_with_handle();
        });
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn panics_when_called_spawn_with_handle_on_remote_handle() {
        let result = panic::catch_unwind(|| {
            let handle = sleep(Duration::from_secs(2)).spawn_with_handle();
            let _handle_to_a_handle = handle.spawn_with_handle();
        });

        if cfg!(debug_assertions) {
            assert!(
                result.is_err(),
                "Spawning a remote handle into a separate task should panic_in_debug_mode"
            );
        } else {
            assert!(result.is_ok(), "Do not panic in release mode");
        }
    }
}
