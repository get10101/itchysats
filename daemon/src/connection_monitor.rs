use std::time::Duration;

use anyhow::Result;
use futures::channel::mpsc;
use futures::future::{AbortHandle, Abortable};
use futures::{SinkExt, StreamExt};
use tokio::sync::watch;
use tokio::time::sleep;
use xtra_productivity::xtra_productivity;

/// Actor encapsulating monitoring of a "connection".
/// Connection is deemed active if the heartbeat is sent before the
/// `timeout_interval` lapses.
pub struct Actor {
    timeout_sender: mpsc::UnboundedSender<ConnectionStatus>,
    timeout_interval: Duration,
    abort_handler: Option<AbortHandle>,
}

/// Message that needs to be periodically sent to keep the connection alive
pub struct Heartbeat;

/// Signal whether the connection is active
#[derive(Clone, Debug, PartialEq)]
pub enum ConnectionStatus {
    Online,
    Offline,
}

impl Actor {
    /// It is expected that receiver of the watch channel outlives the actor
    pub async fn new(
        maker_online_status_feed_sender: watch::Sender<ConnectionStatus>,
        timeout_interval: Duration,
    ) -> Self {
        let (timeout_sender, mut rx) = mpsc::unbounded::<ConnectionStatus>();

        tokio::spawn(async move {
            loop {
                let status = rx.select_next_some().await;
                maker_online_status_feed_sender
                    .send(status)
                    .expect("receiver to outlive the actor");
            }
        });
        Self {
            timeout_interval,
            timeout_sender,
            abort_handler: None,
        }
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle_broadcast_heartbeat(&mut self, _msg: Heartbeat) -> Result<()> {
        self.timeout_sender.send(ConnectionStatus::Online).await?;

        if let Some(abort_handler) = &self.abort_handler {
            abort_handler.abort();
        }
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        self.abort_handler.replace(abort_handle);

        let mut sender = self.timeout_sender.clone();
        let timeout_interval = self.timeout_interval;
        let send_delayed = async move {
            sleep(timeout_interval).await;
            sender
                .send(ConnectionStatus::Offline)
                .await
                .expect("receiver to outlive this task");
        };

        tokio::spawn(async move { Abortable::new(send_delayed, abort_registration).await });
        Ok(())
    }
}

impl xtra::Actor for Actor {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection_monitor;
    use crate::tokio_ext::FutureExt;
    use anyhow::Context;
    use xtra::spawn::TokioGlobalSpawnExt;
    use xtra::Actor as _;

    // TODO: remove this copy-paste from happy_path.rs and extract to reusable place
    /// Returns watch channel value upon change
    async fn next<T>(rx: &mut watch::Receiver<T>) -> T
    where
        T: Clone,
    {
        // TODO: Make timeout configurable, only contract setup can take up to 2 min on CI
        rx.changed()
            .timeout(Duration::from_secs(5))
            .await
            .context("Waiting for next element in channel is taking too long, aborting")
            .unwrap()
            .unwrap();
        rx.borrow().clone()
    }

    #[tokio::test]
    async fn detect_intermittent_connection_loss() {
        let (sender, mut receiver) = watch::channel::<ConnectionStatus>(ConnectionStatus::Offline);

        let timeout_interval = Duration::from_secs(2);

        let maker_monitor_addr = Actor::new(sender, timeout_interval)
            .await
            .create(None)
            .spawn_global();

        // Send the first heartbeat
        maker_monitor_addr
            .send(connection_monitor::Heartbeat)
            .await
            .unwrap()
            .unwrap();

        println!("Monitored entity should be online after heartbeat");
        assert_eq!(next(&mut receiver).await, ConnectionStatus::Online);

        // Sleep for longer than the timeout, simulating going offline
        sleep(timeout_interval * 2).await;

        println!("Monitored entity should be offline after timeout trigger");
        assert_eq!(next(&mut receiver).await, ConnectionStatus::Offline);

        // Send another heartbeat, simulating returning online
        maker_monitor_addr
            .send(connection_monitor::Heartbeat)
            .await
            .unwrap()
            .unwrap();

        println!("Monitored entity should be online after second heartbeat");
        assert_eq!(next(&mut receiver).await, ConnectionStatus::Online);
    }
}
