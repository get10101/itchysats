use crate::SendAsyncSafe;
use async_trait::async_trait;
use futures::Future;
use tokio_extras::TaskMap;
use uuid::Uuid;
use xtra::Context;
use xtra_productivity::xtra_productivity;

/// A spawner actor controls the lifetime of tasks it is instructed to
/// spawn. If all (strong) `Address`es to this `xtra::Actor` are
/// dropped, all the tasks it is tracking will be cancelled.
///
/// Furthermore, a spawner actor will learn about any spawned task
/// that has finished, dropping the corresponding task handle and
/// freeing up memory.
pub struct Actor {
    tasks: TaskMap<Uuid>,
}

impl Actor {
    pub fn new() -> Self {
        Self {
            tasks: TaskMap::default(),
        }
    }
}

#[xtra_productivity]
impl<T> Actor
where
    T: Future<Output = ()> + Send + 'static,
{
    fn handle<T>(&mut self, msg: Spawn<T>, ctx: &mut Context<Self>)
    where
        T: Future<Output = ()> + Send + 'static,
    {
        let this = ctx.address().expect("we are alive");
        let id = Uuid::new_v4();

        self.tasks.add(id, async move {
            msg.task.await;
            if let Err(e) = this.send_async_safe(Finished(id)).await {
                tracing::warn!("Failed to tell spawner about finished task: {e:#}");
            };
        })
    }
}

#[async_trait]
impl<T, EH, EF, E> xtra::Handler<SpawnFallible<T, EH>> for Actor
where
    T: Future<Output = Result<(), E>> + Send + 'static,
    EH: FnOnce(E) -> EF + Send + 'static,
    EF: Future<Output = ()> + Send + 'static,
    E: Send + 'static,
{
    type Return = ();

    async fn handle(&mut self, msg: SpawnFallible<T, EH>, ctx: &mut Context<Self>) {
        let this = ctx.address().expect("we are alive");
        let id = Uuid::new_v4();

        self.tasks.add_fallible(
            id,
            async move {
                msg.task.await?;
                if let Err(e) = this.send_async_safe(Finished(id)).await {
                    tracing::warn!("Failed to tell spawner about finished task: {e:#}",);
                };

                Ok(())
            },
            msg.err_handler,
        )
    }
}

#[xtra_productivity]
impl Actor {
    fn handle(&mut self, Finished(id): Finished) {
        self.tasks.remove(&id);
    }
}

/// Spawn a `task` on the runtime, remembering the handle.
pub struct Spawn<T> {
    task: T,
}

/// Spawn a fallible task `T` on the runtime, remembering the handle.
///
/// If the tasks fails, the `err_handler` will be invoked.
pub struct SpawnFallible<T, EH> {
    task: T,
    err_handler: EH,
}

impl<T, EH> SpawnFallible<T, EH> {
    pub fn new(task: T, err_handler: EH) -> Self {
        Self { task, err_handler }
    }
}

/// Module private message to notify ourselves that a task has
/// finished.
///
/// The provided key of type `Uuid` will be used to remove the
/// corresponding task handle from the internal `TaskMap`.
struct Finished(Uuid);

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

impl Default for Actor {
    fn default() -> Self {
        Self::new()
    }
}
