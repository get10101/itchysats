use futures::FutureExt;
use std::future::Future;
use xtra::refcount::RefCounter;
use xtra::Address;

pub fn spawn<A, Rc, F>(addr: &Address<A, Rc>, fut: F)
where
    Rc: RefCounter,
    F: Future + Send + 'static,
{
    #[allow(clippy::disallowed_methods)]
    tokio::spawn(xtra::scoped(addr, fut.map(|_| ())));
}

pub fn spawn_fallible<A, Rc, Task, Ok, Err, Fn, FnFut>(
    addr: &Address<A, Rc>,
    fut: Task,
    handle_err: Fn,
) where
    Rc: RefCounter,
    Task: Future<Output = Result<Ok, Err>> + Send + 'static,
    Fn: FnOnce(Err) -> FnFut + Send + 'static,
    FnFut: Future + Send,
    Ok: Send,
    Err: Send,
{
    #[allow(clippy::disallowed_methods)]
    tokio::spawn(xtra::scoped(addr, async {
        if let Err(e) = fut.await {
            handle_err(e).await;
        }
    }));
}
