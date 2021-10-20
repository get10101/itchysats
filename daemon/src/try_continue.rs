/// Wrapper for errors in loop that logs error and continues
#[macro_export]
macro_rules! try_continue {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(e) => {
                tracing::error!("{:#}", e);
                continue;
            }
        }
    };
}
