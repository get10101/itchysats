use std::fmt;
use std::future::Future;

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
