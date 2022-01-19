use crate::xtra_ext::ActorName;
use crate::Tasks;
use async_trait::async_trait;
use std::fmt;
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
        let actor = T::name();
        tracing::info!("Spawning new instance of {actor}");

        let this = ctx.address().expect("we are alive");
        let actor = (self.ctor)(this);

        self.metrics.num_spawns += 1;
        self.tasks.add(self.context.attach(actor));
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
        let reason = msg.reason;

        tracing::info!("{actor} stopped: {reason}");

        let should_restart = (self.restart_policy)(reason);

        tracing::debug!(
            "Restart {actor}? {}",
            match should_restart {
                true => "yes",
                false => "no",
            }
        );

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
    use xtra::Actor as _;

    #[tokio::test]
    async fn supervisor_tracks_spawn_metrics() {
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
}
