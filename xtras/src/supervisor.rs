use crate::ActorName;
use async_trait::async_trait;
use futures::FutureExt;
use std::any::Any;
use std::fmt;
use std::panic::AssertUnwindSafe;
use tokio_tasks::Tasks;
use xtra::Address;
use xtra::Context;
use xtra::Message;
use xtra_productivity::xtra_productivity;

/// A supervising actor reacts to messages from the actor it is supervising and restarts it based on
/// a given policy.
pub struct Actor<T, R> {
    context: Context<T>,
    ctor: Box<dyn Fn(Address<Self>) -> T + Send + 'static>,
    tasks: Tasks,
    restart_policy: Box<dyn FnMut(R) -> bool + Send + 'static>,
    metrics: Metrics,
}

#[derive(Default, Clone, Copy)]
struct Metrics {
    /// How many times the supervisor spawned an instance of the actor.
    pub num_spawns: u64,
    /// How many times the actor shut down due to a panic.
    pub num_panics: u64,
}

impl<T, R> Actor<T, R>
where
    T: xtra::Actor,
    R: fmt::Display + fmt::Debug + 'static,
{
    /// Construct a new supervisor.
    ///
    /// The supervisor needs to know two things:
    /// 1. How to construct an instance of the actor.
    /// 2. When to construct an instance of the actor.
    pub fn new(
        ctor: impl (Fn(Address<Self>) -> T) + Send + 'static,
        restart_policy: impl (FnMut(R) -> bool) + Send + 'static,
    ) -> (Self, Address<T>) {
        let (address, context) = Context::new(None);

        let supervisor = Self {
            context,
            ctor: Box::new(ctor),
            tasks: Tasks::default(),
            restart_policy: Box::new(restart_policy),
            metrics: Metrics::default(),
        };

        (supervisor, address)
    }

    fn spawn_new(&mut self, ctx: &mut Context<Self>) {
        let actor_name = T::name();
        tracing::info!(actor = %&actor_name, "Spawning new actor instance");

        let this = ctx.address().expect("we are alive");
        let actor = (self.ctor)(this.clone());

        self.metrics.num_spawns += 1;
        self.tasks.add({
            let task = self.context.attach(actor);

            async move {
                match AssertUnwindSafe(task).catch_unwind().await {
                    Ok(()) => {
                        tracing::warn!(actor = %&actor_name, "Actor stopped without sending a `Stopped` message");
                    }
                    Err(error) => {
                        let _ = this.send(Panicked { error }).await;
                    }
                }
            }
        });
    }
}

#[async_trait]
impl<T, R> xtra::Actor for Actor<T, R>
where
    T: xtra::Actor,
    R: fmt::Display + fmt::Debug + 'static,
{
    async fn started(&mut self, ctx: &mut Context<Self>) {
        self.spawn_new(ctx);
    }
}

#[xtra_productivity(message_impl = false)]
impl<T, R> Actor<T, R>
where
    T: xtra::Actor,
    R: fmt::Display + fmt::Debug + 'static,
{
    pub fn handle(&mut self, msg: Stopped<R>, ctx: &mut Context<Self>) {
        let actor = T::name();
        let reason_str = msg.reason.to_string();
        let should_restart = (self.restart_policy)(msg.reason);

        tracing::info!(actor = %&actor, reason = %reason_str, restart = %should_restart, "Actor stopped");

        if should_restart {
            self.spawn_new(ctx)
        }
    }
}

#[xtra_productivity]
impl<T, R> Actor<T, R>
where
    T: xtra::Actor,
    R: fmt::Display + fmt::Debug + 'static,
{
    pub fn handle(&mut self, _: GetMetrics) -> Metrics {
        self.metrics
    }
}

#[async_trait]
impl<T, R> xtra::Handler<Panicked> for Actor<T, R>
where
    T: xtra::Actor,
    R: fmt::Display + fmt::Debug + 'static,
{
    async fn handle(&mut self, msg: Panicked, ctx: &mut Context<Self>) {
        let actor = T::name();
        let reason = match msg.error.downcast::<&'static str>() {
            Ok(reason) => *reason,
            Err(_) => "unknown",
        };

        tracing::info!(actor = %&actor, %reason, restart = true, "Actor panicked");

        self.metrics.num_panics += 1;
        self.spawn_new(ctx)
    }
}

/// Tell the supervisor that the actor was stopped.
///
/// The given `reason` will be passed to the `restart_policy` configured in the supervisor. If it
/// yields `true`, a new instance of the actor will be spawned.
#[derive(Debug)]
pub struct Stopped<R> {
    pub reason: R,
}

impl<R: fmt::Debug + Send + 'static> Message for Stopped<R> {
    type Result = ();
}

/// Module private message to notify ourselves that an actor panicked.
#[derive(Debug)]
struct Panicked {
    pub error: Box<dyn Any + Send>,
}

impl xtra::Message for Panicked {
    type Result = ();
}

/// Return the metrics tracked by this supervisor.
///
/// Currently private because it is a feature only used for testing. If we want to expose metrics
/// about the supervisor, we should look into creating a [`tracing::Subscriber`] that processes the
/// events we are emitting.
#[derive(Debug)]
struct GetMetrics;

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::util::SubscriberInitExt;
    use xtra::Actor as _;

    #[tokio::test]
    async fn supervisor_tracks_spawn_metrics() {
        let _guard = tracing_subscriber::fmt().with_test_writer().set_default();

        let (supervisor, address) =
            Actor::new(|supervisor| RemoteShutdown { supervisor }, |_| true);
        let (supervisor, task) = supervisor.create(None).run();

        #[allow(clippy::disallowed_method)]
        tokio::spawn(task);

        let metrics = supervisor.send(GetMetrics).await.unwrap();
        assert_eq!(
            metrics.num_spawns, 1,
            "after initial spawn, should have 1 spawn"
        );

        address.send(Shutdown).await.unwrap();

        let metrics = supervisor.send(GetMetrics).await.unwrap();
        assert_eq!(
            metrics.num_spawns, 2,
            "after shutdown, should have 2 spawns"
        );
    }

    #[tokio::test]
    async fn supervisor_tracks_panic_metrics() {
        let _guard = tracing_subscriber::fmt().with_test_writer().set_default();

        std::panic::set_hook(Box::new(|_| ())); // Override hook to avoid panic printing to log.

        let (supervisor, address) = Actor::new(
            |supervisor| PanickingActor {
                _supervisor: supervisor,
            },
            |_| true,
        );
        let (supervisor, task) = supervisor.create(None).run();

        #[allow(clippy::disallowed_method)]
        tokio::spawn(task);

        address.send(Panic).await.unwrap_err(); // Actor will be dead by the end of the function call because it panicked.

        let metrics = supervisor.send(GetMetrics).await.unwrap();
        assert_eq!(metrics.num_spawns, 2, "after panic, should have 2 spawns");
        assert_eq!(metrics.num_panics, 1, "after panic, should have 1 panic");
    }

    /// An actor that can be shutdown remotely.
    struct RemoteShutdown {
        supervisor: Address<Actor<Self, String>>,
    }

    #[derive(Debug)]
    struct Shutdown;

    #[async_trait]
    impl xtra::Actor for RemoteShutdown {
        async fn stopped(self) {
            self.supervisor
                .send(Stopped {
                    reason: String::new(),
                })
                .await
                .unwrap();
        }
    }

    #[xtra_productivity]
    impl RemoteShutdown {
        fn handle(&mut self, _: Shutdown, ctx: &mut xtra::Context<Self>) {
            ctx.stop()
        }
    }

    struct PanickingActor {
        _supervisor: Address<Actor<Self, String>>,
    }

    #[derive(Debug)]
    struct Panic;

    impl xtra::Actor for PanickingActor {}

    #[xtra_productivity]
    impl PanickingActor {
        fn handle(&mut self, _: Panic) {
            panic!("Help!")
        }
    }
}
