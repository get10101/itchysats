use std::ops::ControlFlow;
use std::time::Duration;
use tokio::time::timeout;
use xtra::Actor;
use xtra::Context;

pub trait HandlerTimeoutExt<A> {
    fn with_handler_timeout(self, timeout: Duration) -> TimeoutManager<A>;
}

impl<A> HandlerTimeoutExt<A> for Context<A> {
    fn with_handler_timeout(self, timeout: Duration) -> TimeoutManager<A> {
        TimeoutManager { ctx: self, timeout }
    }
}

pub struct TimeoutManager<A> {
    ctx: Context<A>,
    timeout: Duration,
}

impl<A> TimeoutManager<A>
where
    A: Actor,
{
    pub async fn run(mut self, mut actor: A) -> A::Stop {
        loop {
            let msg = self.ctx.next_message().await;

            let (span, fut) = self.ctx.tick_instrumented(msg, &mut actor);
            match timeout(self.timeout, fut).await {
                Ok(ControlFlow::Continue(())) => (),
                Ok(ControlFlow::Break(())) => break actor.stopped().await,
                Err(_elapsed) => {
                    if let Some(span) = span {
                        let _entered = span.enter();
                        let timeout_seconds = self.timeout.as_secs();
                        span.record("interrupted", &"timed_out");
                        tracing::warn!(%timeout_seconds, "Handler execution timed out");
                    }
                }
            }
        }
    }
}
