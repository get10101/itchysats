use crate::ActorName;
use async_trait::async_trait;
use futures::FutureExt;
use std::any::Any;
use std::error::Error;
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
    ctor: Box<dyn Fn() -> T + Send + 'static>,
    tasks: Tasks,
    restart_policy: Box<dyn FnMut(&R) -> bool + Send + 'static>,
    metrics: Metrics,
}

#[derive(Default, Clone, Copy)]
struct Metrics {
    /// How many times the supervisor spawned an instance of the actor.
    pub num_spawns: u64,
    /// How many times the actor shut down due to a panic.
    pub num_panics: u64,
}

#[derive(Debug)]
struct UnitReason {}

impl fmt::Display for UnitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "()")
    }
}

impl Error for UnitReason {}

impl From<()> for UnitReason {
    fn from(_: ()) -> Self {
        UnitReason {}
    }
}

impl<T> Actor<T, UnitReason>
where
    T: xtra::Actor<Stop = ()>,
{
    /// Construct a new supervisor for an [`Actor`] with an [`xtra::Actor::Stop`] value of `()`.
    ///
    /// The actor will always be restarted if it stops. If you don't want this behaviour, don't use
    /// a supervisor. If you want more fine-granular control in which circumstances the actor
    /// should be restarted, set [`xtra::Actor::Stop`] to a more descriptive value and use
    /// [`Actor::with_policy`].
    pub fn new(ctor: impl (Fn() -> T) + Send + 'static) -> (Self, Address<T>) {
        let (address, context) = Context::new(None);

        let supervisor = Self {
            context,
            ctor: Box::new(ctor),
            tasks: Tasks::default(),
            restart_policy: Box::new(|UnitReason {}| true),
            metrics: Metrics::default(),
        };

        (supervisor, address)
    }
}

impl<T, R, S> Actor<T, R>
where
    T: xtra::Actor<Stop = S>,
    R: Error + Send + Sync + 'static,
    S: Into<R> + Send + 'static,
{
    /// Construct a new supervisor.
    ///
    /// The supervisor needs to know two things:
    /// 1. How to construct an instance of the actor.
    /// 2. When to construct an instance of the actor.
    pub fn with_policy(
        ctor: impl (Fn() -> T) + Send + 'static,
        restart_policy: impl (FnMut(&R) -> bool) + Send + 'static,
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
        let actor = (self.ctor)();

        self.metrics.num_spawns += 1;
        self.tasks.add({
            let task = self.context.attach(actor);

            async move {
                match AssertUnwindSafe(task).catch_unwind().await {
                    Ok(reason) => {
                        let _ = this
                            .send(Stopped {
                                reason: reason.into(),
                            })
                            .await;
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
impl<T, R, S> xtra::Actor for Actor<T, R>
where
    T: xtra::Actor<Stop = S>,
    R: Error + Send + Sync + 'static,
    S: Into<R> + Send + 'static,
{
    type Stop = ();

    async fn started(&mut self, ctx: &mut Context<Self>) {
        self.spawn_new(ctx);
    }

    async fn stopped(self) -> Self::Stop {}
}

#[xtra_productivity(message_impl = false)]
impl<T, R, S> Actor<T, R>
where
    T: xtra::Actor<Stop = S>,
    R: Error + Send + Sync + 'static,
    S: Into<R> + Send + 'static,
{
    pub fn handle(&mut self, msg: Stopped<R>, ctx: &mut Context<Self>) {
        let actor = T::name();
        let should_restart = (self.restart_policy)(&msg.reason);
        let reason_str = format!("{:#}", anyhow::Error::new(msg.reason)); // Anyhow will format the entire chain of errors when using `alternate` Display (`#`)

        tracing::info!(actor = %&actor, reason = %reason_str, restart = %should_restart, "Actor stopped");

        if should_restart {
            self.spawn_new(ctx)
        }
    }
}

#[xtra_productivity]
impl<T, R, S> Actor<T, R>
where
    T: xtra::Actor<Stop = S>,
    R: Error + Send + Sync + 'static,
    S: Into<R>,
{
    pub fn handle(&mut self, _: GetMetrics) -> Metrics {
        self.metrics
    }
}

#[async_trait]
impl<T, R, S> xtra::Handler<Panicked> for Actor<T, R>
where
    T: xtra::Actor<Stop = S>,
    R: Error + Send + Sync + 'static,
    S: Into<R> + Send + 'static,
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

/// Module private message to notify ourselves that an actor stopped.
///
/// The given `reason` will be passed to the `restart_policy` configured in the supervisor. If it
/// yields `true`, a new instance of the actor will be spawned.
#[derive(Debug)]
struct Stopped<R> {
    pub reason: R,
}

impl<R: Send + 'static> Message for Stopped<R> {
    type Result = ();
}

/// Module private message to notify ourselves that an actor panicked.
#[derive(Debug)]
struct Panicked {
    pub error: Box<dyn Any + Send>,
}

impl Message for Panicked {
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
    use std::io;
    use tracing_subscriber::util::SubscriberInitExt;
    use xtra::Actor as _;

    #[tokio::test]
    async fn supervisor_tracks_spawn_metrics() {
        let _guard = tracing_subscriber::fmt().with_test_writer().set_default();

        let (supervisor, address) = Actor::with_policy(|| RemoteShutdown, |_: &io::Error| true);
        let (supervisor, task) = supervisor.create(None).run();

        #[allow(clippy::disallowed_methods)]
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
    async fn restarted_actor_is_usable() {
        let _guard = tracing_subscriber::fmt().with_test_writer().set_default();

        let (supervisor, address) = Actor::with_policy(|| RemoteShutdown, |_: &io::Error| true);
        let (_supervisor, task) = supervisor.create(None).run();

        #[allow(clippy::disallowed_methods)]
        tokio::spawn(task);

        address.send(Shutdown).await.unwrap();

        let message = address.send(SayHello("World".to_owned())).await.unwrap();

        assert_eq!(message, "Hello World");
    }

    #[tokio::test]
    async fn supervisor_tracks_panic_metrics() {
        let _guard = tracing_subscriber::fmt().with_test_writer().set_default();

        std::panic::set_hook(Box::new(|_| ())); // Override hook to avoid panic printing to log.

        let (supervisor, address) = Actor::with_policy(|| PanickingActor, |_: &io::Error| true);
        let (supervisor, task) = supervisor.create(None).run();

        #[allow(clippy::disallowed_methods)]
        tokio::spawn(task);

        address.send(Panic).await.unwrap_err(); // Actor will be dead by the end of the function call because it panicked.

        let metrics = supervisor.send(GetMetrics).await.unwrap();
        assert_eq!(metrics.num_spawns, 2, "after panic, should have 2 spawns");
        assert_eq!(metrics.num_panics, 1, "after panic, should have 1 panic");
    }

    #[tokio::test]
    async fn supervisor_can_supervise_unit_actor() {
        let _guard = tracing_subscriber::fmt().with_test_writer().set_default();

        let (supervisor, _address) = Actor::new(|| UnitActor);
        let (_supervisor, task) = supervisor.create(None).run();

        #[allow(clippy::disallowed_methods)]
        tokio::spawn(task);
    }

    /// An actor that can be shutdown remotely.
    struct RemoteShutdown;

    #[derive(Debug)]
    struct Shutdown;

    struct SayHello(String);

    #[async_trait]
    impl xtra::Actor for RemoteShutdown {
        type Stop = io::Error;

        async fn stopped(self) -> Self::Stop {
            io::Error::new(io::ErrorKind::Other, "unknown")
        }
    }

    #[xtra_productivity]
    impl RemoteShutdown {
        fn handle(&mut self, _: Shutdown, ctx: &mut Context<Self>) {
            ctx.stop()
        }

        fn handle(&mut self, msg: SayHello) -> String {
            format!("Hello {}", msg.0)
        }
    }

    struct PanickingActor;

    #[derive(Debug)]
    struct Panic;

    #[async_trait]
    impl xtra::Actor for PanickingActor {
        type Stop = io::Error;

        async fn stopped(self) -> Self::Stop {
            io::Error::new(io::ErrorKind::Other, "unknown")
        }
    }

    #[xtra_productivity]
    impl PanickingActor {
        fn handle(&mut self, _: Panic) {
            panic!("Help!")
        }
    }

    struct UnitActor;

    #[async_trait]
    impl xtra::Actor for UnitActor {
        type Stop = ();

        async fn stopped(self) -> Self::Stop {}
    }
}
