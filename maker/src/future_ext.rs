use std::future::Future;
use std::time::Duration;
use tokio::time::timeout;
use tokio::time::Timeout;

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
