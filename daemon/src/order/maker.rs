use crate::command;
use crate::oracle;
use crate::order::protocol;
use crate::order::protocol::MakerMessage;
use crate::order::protocol::TakerMessage;
use crate::process_manager;
use crate::projection;
use crate::setup_contract;
use crate::wallet;
use crate::wire::SetupMsg;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use asynchronous_codec::Framed;
use asynchronous_codec::JsonCodec;
use futures::channel::oneshot;
use futures::future;
use futures::SinkExt;
use futures::StreamExt;
use maia_core::secp256k1_zkp::schnorrsig;
use model::Cfd;
use model::Identity;
use model::OrderId;
use model::Role;
use model::Usd;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::fmt;
use xtra::prelude::MessageChannel;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor as _;
use xtra_libp2p::NewInboundSubstream;
use xtra_productivity::xtra_productivity;
use xtras::spawner;
use xtras::spawner::SpawnFallible;
use xtras::SendAsyncSafe;

pub struct Actor {
    executor: command::Executor,
    oracle_pk: schnorrsig::PublicKey,
    get_announcement: Box<dyn MessageChannel<oracle::GetAnnouncement>>,
    build_party_params: Box<dyn MessageChannel<wallet::BuildPartyParams> + 'static>,
    sign: Box<dyn MessageChannel<wallet::Sign> + 'static>,
    projection: xtra::Address<projection::Actor>,
    n_payouts: usize,
    decision_senders: HashMap<OrderId, oneshot::Sender<protocol::Decision>>,
    db: sqlite_db::Connection,
    spawner: xtra::Address<spawner::Actor>,
}

impl Actor {
    pub fn new(
        n_payouts: usize,
        oracle_pk: schnorrsig::PublicKey,
        get_announcement: &(impl MessageChannel<oracle::GetAnnouncement> + 'static),
        (db, process_manager): (sqlite_db::Connection, xtra::Address<process_manager::Actor>),
        (build_party_params, sign): (
            &(impl MessageChannel<wallet::BuildPartyParams> + 'static),
            &(impl MessageChannel<wallet::Sign> + 'static),
        ),
        projection: xtra::Address<projection::Actor>,
    ) -> Self {
        let spawner = spawner::Actor::new().create(None).spawn_global();

        Self {
            executor: command::Executor::new(db.clone(), process_manager),
            oracle_pk,
            get_announcement: get_announcement.clone_channel(),
            build_party_params: build_party_params.clone_channel(),
            sign: sign.clone_channel(),
            projection,
            n_payouts,
            decision_senders: HashMap::default(),
            db,
            spawner,
        }
    }

    pub async fn handle_new_inbound_stream(&mut self, msg: NewInboundSubstream) -> Result<()> {
        let NewInboundSubstream { peer, stream } = msg;

        let mut framed = Framed::new(stream, JsonCodec::<MakerMessage, TakerMessage>::new());

        let (
            id,
            contracts,
            leverage,
            position,
            opening_price,
            settlement_interval,
            opening_fee,
            funding_rate,
            tx_fee_rate,
            oracle_event_id,
        ) = match framed.next().await.context("Stream terminated")?? {
            TakerMessage::PlaceOrder {
                id,
                contracts,
                leverage,
                position,
                opening_price,
                settlement_interval,
                opening_fee,
                funding_rate,
                tx_fee_rate,
                oracle_event_id,
            } => (
                id,
                contracts,
                leverage,
                position,
                opening_price,
                settlement_interval,
                opening_fee,
                funding_rate,
                tx_fee_rate,
                oracle_event_id,
            ),
            TakerMessage::ContractSetupMsg(_) => bail!("Unexpected message"),
        };

        tracing::info!(taker = %peer, %contracts, order_id = %id, "Taker wants to place an order");

        let cfd = Cfd::new(
            id,
            position,
            opening_price,
            leverage,
            settlement_interval,
            Role::Maker,
            Usd::new(Decimal::from(u64::from(contracts))),
            // Completely irrelevant when using libp2p
            Identity::new(x25519_dalek::PublicKey::from(
                *b"hello world, oh what a beautiful",
            )),
            Some(peer.into()),
            opening_fee,
            funding_rate,
            tx_fee_rate,
        );

        // TODO: If this fails we shouldn't try to append
        // `ContractSetupFailed` to the nonexistent CFD
        self.db.insert_cfd(&cfd).await?;

        self.projection
            .send_async_safe(projection::CfdChanged(cfd.id()))
            .await?;

        let (sender, receiver) = oneshot::channel();
        self.decision_senders.insert(id, sender);

        let task = {
            let build_party_params = self.build_party_params.clone_channel();
            let sign = self.sign.clone_channel();
            let get_announcement = self.get_announcement.clone_channel();
            let executor = self.executor.clone();
            let oracle_pk = self.oracle_pk;
            let n_payouts = self.n_payouts;
            async move {
                match receiver.await? {
                    protocol::Decision::Accept => {
                        framed
                            .send(MakerMessage::Decision(protocol::Decision::Accept))
                            .await?;

                        tracing::info!(taker = %peer, %contracts, order_id = %id, "Order accepted");
                    }
                    protocol::Decision::Reject => {
                        framed
                            .send(MakerMessage::Decision(protocol::Decision::Reject))
                            .await?;

                        tracing::info!(taker = %peer, %contracts, order_id = %id, "Order rejected");

                        executor
                            .execute(id, |cfd| {
                                cfd.reject_contract_setup(anyhow::anyhow!("Unknown"))
                            })
                            .await?;

                        return anyhow::Ok(());
                    }
                }

                let (setup_params, position) = executor
                    .execute(id, |cfd| cfd.start_contract_setup())
                    .await?;

                let (sink, stream) = framed.split();

                let announcement = get_announcement
                    .send(oracle::GetAnnouncement(oracle_event_id))
                    .await??;

                let dlc = setup_contract::new(
                    sink.with(|msg| future::ok(MakerMessage::ContractSetupMsg(Box::new(msg)))),
                    Box::pin(stream.filter_map(|msg| async move {
                        let msg = match msg {
                            Ok(msg) => msg,
                            Err(e) => {
                                tracing::error!("Failed to deserialize MakerMessage: {e:#}");
                                return None;
                            }
                        };

                        match SetupMsg::try_from(msg) {
                            Ok(msg) => Some(msg),
                            Err(e) => {
                                tracing::error!("Failed to convert to SetupMsg: {e:#}");
                                None
                            }
                        }
                    }))
                    .fuse(),
                    (oracle_pk, announcement),
                    setup_params,
                    build_party_params,
                    sign,
                    Role::Maker,
                    position,
                    n_payouts,
                )
                .await?;

                if let Err(e) = executor
                    .execute(id, |cfd| cfd.complete_contract_setup(dlc))
                    .await
                {
                    tracing::error!(%id, "Failed to execute contract_setup_completed: {e:#}");
                }

                anyhow::Ok(())
            }
        };

        let err_handler = {
            let executor = self.executor.clone();
            move |e| async move {
                if let Err(e) = executor
                    .execute(id, |cfd| Ok(cfd.fail_contract_setup(e)))
                    .await
                {
                    tracing::error!(%id, "Failed to execute fail_contract_setup: {e:#}");
                }
            }
        };

        if let Err(e) = self
            .spawner
            .send_async_safe(SpawnFallible::new(task, err_handler))
            .await
        {
            tracing::error!("Failed to spawn task to take order: {e:#}");
        }

        Ok(())
    }
}

#[xtra_productivity(message_impl = false)]
impl Actor {
    async fn handle(&mut self, msg: NewInboundSubstream) {
        if let Err(e) = self.handle_new_inbound_stream(msg).await {
            tracing::error!("Failed to handle new inbound substream: {e:#}");
        }
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: Decision) -> Result<()> {
        let id = msg.id();

        tracing::debug!("Instructed to {msg} order {id}");

        let sender = self
            .decision_senders
            .remove(&id)
            .context("Can't make decision on nonexistent order {id}")?;

        sender
            .send(msg.into())
            .map_err(|_| anyhow!("Can't deliver decision on taking order {id}"))?;

        Ok(())
    }
}

#[derive(Clone, Copy)]
pub enum Decision {
    Accept(OrderId),
    Reject(OrderId),
}

impl Decision {
    fn id(&self) -> OrderId {
        match self {
            Decision::Accept(id) | Decision::Reject(id) => *id,
        }
    }
}

impl From<Decision> for protocol::Decision {
    fn from(decision: Decision) -> Self {
        match decision {
            Decision::Accept(_) => protocol::Decision::Accept,
            Decision::Reject(_) => protocol::Decision::Reject,
        }
    }
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Decision::Accept(_) => "accept",
            Decision::Reject(_) => "reject",
        };

        s.fmt(f)
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
