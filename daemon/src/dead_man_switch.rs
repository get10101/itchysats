use async_trait::async_trait;
use std::time::{Duration, SystemTime};
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;

/// An actor that dies once its heart stops beating.
pub struct Actor {
    rx: Box<dyn MessageChannel<Died>>,
    /// Max duration since the last heartbeat until we die.
    timeout: Duration,
    last_heartbeat: SystemTime,
}

/// Message to trigger a single heartbeat.
///
/// If a [`Heartbeat`] is not triggered within the configured pulse duration, the actor dies.
pub struct Heartbeat;

/// Message sent as soon as the actor dies.
pub struct Died;

impl xtra::Message for Died {
    type Result = ();
}

/// Private message to measure the current pulse (i.e. check when we received the last heartbeat).
struct MeasurePulse;

impl Actor {
    pub fn new(rx: Box<dyn MessageChannel<Died>>, timeout: Duration) -> Self {
        Self {
            rx,
            timeout,
            last_heartbeat: SystemTime::now(),
        }
    }
}

#[xtra_productivity]
impl Actor {
    fn handle_heartbeat(&mut self, _: Heartbeat) {
        self.last_heartbeat = SystemTime::now();
    }

    fn handle_measure_pulse(&mut self, _: MeasurePulse, ctx: &mut xtra::Context<Self>) {
        let time_since_last_heartbeat = SystemTime::now()
            .duration_since(self.last_heartbeat)
            .expect("now is always later than heartbeat");

        if time_since_last_heartbeat > self.timeout {
            ctx.stop();
        }
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let future = ctx
            .notify_interval(self.timeout, || MeasurePulse)
            .expect("we just started");

        tokio::spawn(future);
    }

    async fn stopped(self) {
        // we don't care if the receiver already disconnected
        let _ = self.rx.send(Died).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xtra::spawn::TokioGlobalSpawnExt as _;
    use xtra::Actor as _;

    #[tokio::test]
    async fn switch_triggers_on_missing_heartbeat() {
        let (addr, mut ctx) = xtra::Context::new(None);
        let mut dummy = Dummy::default();
        let dead_man_switch = Actor::new(Box::new(addr), KEEP_ALIVE_TIMEOUT)
            .create(None)
            .spawn_global();

        // while we are sending heartbeat, actor does not die
        for _ in 0..2 {
            let _ = dead_man_switch.send(Heartbeat).await;

            ctx.handle_while(&mut dummy, tokio::time::sleep(Duration::from_secs(1)))
                .await;
            assert!(!dummy.received_died);
        }

        // once we pass the deadline, the actor shuts down
        tokio::time::sleep(KEEP_ALIVE_TIMEOUT).await;
        ctx.yield_once(&mut dummy).await;

        assert!(dummy.received_died);
        assert!(!dead_man_switch.is_connected());
    }

    const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(2);

    #[derive(Default)]
    struct Dummy {
        received_died: bool,
    }

    impl xtra::Actor for Dummy {}

    #[xtra_productivity(message_impl = false)]
    impl Dummy {
        fn handle_died(&mut self, _: Died) {
            self.received_died = true
        }
    }
}
