use std::fmt::Debug;
use std::future::Future;
use tracing::{field, Instrument, Span};
use tracing::field::debug;
use xtra::refcount::RefCounter;
use xtra::Address;

fn instrumented_scoped<A, Rc, Task>(
    addr: &Address<A, Rc>,
    fut: Task
) -> impl Future<Output = (Span, Option<Task::Output>)> + 'static
    where Rc: RefCounter,
          Task: Future + Send + 'static,
          Task::Output: Send
{
    let span = tracing::debug_span!(
        "xtra scoped task",
        actor = std::any::type_name::<A>(),
        finished = field::Empty,
        success = field::Empty,
        err = field::Empty,
    );
    let fut = fut.instrument(tracing::debug_span!(parent: &span, "scoped task inner future"));
    let scoped = xtra::scoped(addr, fut).instrument(span.clone());

    async move {
        match scoped.await {
            Some(v) => {
                span.record("finished", &true);
                span.in_scope(|| tracing::debug!("Task finished"));
                let v: Task::Output = v;
                (span, Some(v))
            }
            None => {
                span.record("finished", &false);
                span.in_scope(|| tracing::debug!("Actor lifecycle finished; task cancelled"));
                (span, None)
            }
        }
    }
}

pub fn spawn<A, Rc, F>(addr: &Address<A, Rc>, fut: F)
where
    A: 'static,
    Rc: RefCounter,
    F: Future + Send + 'static,
    F::Output: Send,
{
    let fut = instrumented_scoped(addr, fut);

    #[allow(clippy::disallowed_methods)]
    tokio::spawn(fut);
}

pub fn spawn_fallible<A, Rc, Task, Ok, Err, Fn, FnFut>(
    addr: &Address<A, Rc>,
    fut: Task,
    handle_err: Fn,
) where
    A: 'static,
    Rc: RefCounter,
    Task: Future<Output = Result<Ok, Err>> + Send + 'static,
    Fn: FnOnce(Err) -> FnFut + Send + 'static,
    FnFut: Future + Send,
    Ok: Send,
    Err: Debug + Send,
{
    let fut = instrumented_scoped(addr, fut);

    #[allow(clippy::disallowed_methods)]
    tokio::spawn(async move {
        let (span, res) = fut.await;

        match res {
            Some(Ok(_)) => {
                span.in_scope(|| tracing::debug!("Task finished successfully"));
                span.record("success", &true);
            }
            Some(Err(err)) => {
                span.in_scope(|| tracing::warn!("Task failed"));
                span.record("err", &debug(&err));
                handle_err(err).instrument(tracing::debug_span!(parent: &span, "scoped task handle_err")).await;
            },
            None => (),
        }
    });
}
