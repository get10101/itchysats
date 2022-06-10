use crate::endpoint;
use crate::Endpoint;
use crate::ListenOn;
use anyhow::anyhow;
use async_trait::async_trait;
use libp2p_core::Multiaddr;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::Address;
use xtra_productivity::xtra_productivity;

/// If we're not connected by this time, stop the actor.
pub const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// xtra actor that taker care of listening for incoming connections to the Endpoint.
///
/// Periodically polls Endpoint to check whether connection is still active.
/// Should be used in conjunction with supervisor for continuous and resilient listening.
pub struct Actor {
    tasks: Tasks,
    endpoint: Address<Endpoint>,
    listen_address: Multiaddr,
    is_listening: bool,
    /// Contains the reason we are stopping.
    stop_reason: Option<Error>,
}

impl Actor {
    pub fn new(endpoint: Address<Endpoint>, listen_address: Multiaddr) -> Self {
        Self {
            tasks: Tasks::default(),
            endpoint,
            listen_address,
            is_listening: false,
            stop_reason: None,
        }
    }

    fn stop_with_error(&mut self, e: Error, ctx: &mut xtra::Context<Self>) {
        self.stop_reason = Some(e);
        ctx.stop();
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = Error;

    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let endpoint = self.endpoint.clone();
        let listen_address = self.listen_address.clone();

        if let Err(e) = endpoint.send(ListenOn(listen_address)).await {
            self.stop_with_error(Error::Failed { source: anyhow!(e) }, ctx);
        };

        let this = ctx.address().expect("self to be alive");
        self.tasks.add(async move {
            tokio::time::sleep(CONNECTION_TIMEOUT).await;
            this.send(StopIfNotListening)
                .await
                .expect("to deliver stop message");
        })
    }

    async fn stopped(self) -> Self::Stop {
        self.stop_reason.unwrap_or(Error::Unspecified)
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: Error, ctx: &mut xtra::Context<Self>) {
        self.stop_with_error(msg, ctx);
    }

    async fn handle(&mut self, _msg: StopIfNotListening, ctx: &mut xtra::Context<Self>) {
        if !self.is_listening {
            self.stop_with_error(
                Error::Failed {
                    source: anyhow!("Did not connect in time"),
                },
                ctx,
            )
        }
    }
}

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle(&mut self, msg: endpoint::ListenAddressAdded) {
        if msg.address == self.listen_address {
            self.is_listening = true;
        }
    }

    async fn handle(&mut self, msg: endpoint::ListenAddressRemoved, ctx: &mut xtra::Context<Self>) {
        if msg.address == self.listen_address {
            self.is_listening = false;
            self.stop_with_error(Error::ConnectionDropped, ctx);
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Listener failed")]
    Failed {
        #[from]
        source: anyhow::Error,
    },
    #[error("Endpoint actor is disconnected")]
    NoEndpoint,
    #[error("Connection dropped from endpoint")]
    ConnectionDropped,
    #[error("Stop reason was not specified")]
    Unspecified,
}

struct StopIfNotListening;
