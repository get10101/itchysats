/// Wrapper for handlers to log errors
macro_rules! log_error {
    ($future:expr) => {
        if let Err(e) = $future.await {
            tracing::error!("Message handler failed: {:#}", e);
        }
    };
}
pub(crate) use log_error;
