use crate::identify::protocol;
use crate::identify::PeerInfo;
use crate::identify::PROTOCOL;
use async_trait::async_trait;
use std::collections::HashMap;
use tokio::sync::watch;
use tokio_extras::spawn_fallible;
use xtra::Address;
use xtra::Context;
use xtra_libp2p::endpoint;
use xtra_libp2p::endpoint::RegisterListenProtocols;
use xtra_libp2p::libp2p::PeerId;
use xtra_libp2p::Endpoint;
use xtra_libp2p::OpenSubstream;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    endpoint: Address<Endpoint>,
    peer_infos: HashMap<PeerId, PeerInfo>,
    peer_info_channel: Option<watch::Sender<Option<PeerInfo>>>,
}

impl Actor {
    pub fn new(endpoint: Address<Endpoint>) -> Self {
        LIBP2P_PEER_INFORMATION.reset();

        Self {
            endpoint,
            peer_infos: HashMap::default(),
            peer_info_channel: None,
        }
    }

    pub fn new_with_subscriber(
        endpoint: Address<Endpoint>,
    ) -> (Self, watch::Receiver<Option<PeerInfo>>) {
        LIBP2P_PEER_INFORMATION.reset();

        let (sender, receiver) = watch::channel::<Option<PeerInfo>>(None);

        (
            Self {
                endpoint,
                peer_infos: HashMap::default(),
                peer_info_channel: Some(sender),
            },
            receiver,
        )
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

pub(crate) struct IdentifyMsgReceived {
    peer_id: PeerId,
    identify_msg: protocol::IdentifyMsg,
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: IdentifyMsgReceived) {
        let peer_id = msg.peer_id;
        let peer_info = match PeerInfo::try_from(msg.identify_msg.clone()) {
            Ok(peer_info) => peer_info,
            Err(e) => {
                tracing::error!("Peer info discarded {:?}: {e:#}", msg.identify_msg);
                return;
            }
        };

        let daemon_version = peer_info.daemon_version.clone();
        let environment = peer_info.environment.clone();

        tracing::info!(
            %peer_id,
            %daemon_version,
            %environment,
            protocols = ?peer_info.protocols,
            "New identify message received"
        );

        if self.peer_infos.insert(peer_id, peer_info.clone()).is_none() {
            // Only increment if we don't know the peer info already because sometimes we are not
            // notified about ConnectionDropped
            LIBP2P_PEER_INFORMATION
                .with(&HashMap::from([
                    (DAEMON_VERSION_LABEL, daemon_version.as_str()),
                    (ENVIRONMENT_LABEL, environment.to_string().as_str()),
                ]))
                .inc();
        }

        if let Some(peer_info_channel) = &self.peer_info_channel {
            if let Err(e) = peer_info_channel.send(Some(peer_info)) {
                tracing::warn!("Failed to send identity info to notify channel: {e:#}");
            }
        }
    }

    async fn handle_connections_established(
        &mut self,
        msg: endpoint::ConnectionEstablished,
        ctx: &mut Context<Self>,
    ) {
        let peer_id = msg.peer_id;
        let endpoint = self.endpoint.clone();
        let this = ctx.address().expect("we are alive");

        let request_identify_msg_fut = {
            let this = this.clone();
            async move {
                let stream = endpoint
                    .send(OpenSubstream::single_protocol(peer_id, PROTOCOL))
                    .await??
                    .await?;

                let identify_msg = protocol::recv(stream).await?;

                endpoint
                    .send(RegisterListenProtocols {
                        peer_id,
                        listen_protocols: identify_msg.protocols(),
                    })
                    .await?;

                this.send(IdentifyMsgReceived {
                    peer_id,
                    identify_msg,
                })
                .await?;

                anyhow::Ok(())
            }
        };

        let err_handler = move |e| async move {
            tracing::debug!(%peer_id, "Identify protocol failed upon request: {e:#}")
        };

        spawn_fallible(&this, request_identify_msg_fut, err_handler);
    }

    async fn handle_connections_dropped(&mut self, msg: endpoint::ConnectionDropped) {
        let peer_id = msg.peer_id;
        tracing::trace!(%peer_id, "Remove peer-info because connection dropped");
        if let Some(peer_info) = self.peer_infos.remove(&peer_id) {
            LIBP2P_PEER_INFORMATION
                .with(&HashMap::from([
                    (DAEMON_VERSION_LABEL, peer_info.daemon_version.as_str()),
                    (
                        ENVIRONMENT_LABEL,
                        peer_info.environment.to_string().as_str(),
                    ),
                ]))
                .dec();
        }
    }
}

const DAEMON_VERSION_LABEL: &str = "daemon_version";
const ENVIRONMENT_LABEL: &str = "environment";

static LIBP2P_PEER_INFORMATION: conquer_once::Lazy<prometheus::IntGaugeVec> =
    conquer_once::Lazy::new(|| {
        prometheus::register_int_gauge_vec!(
            "libp2p_peer_information",
            "The number of active libp2p connections broken down by daemon version and environment.",
            &[DAEMON_VERSION_LABEL, ENVIRONMENT_LABEL]
        )
        .unwrap()
    });
