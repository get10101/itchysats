use crate::command;
use crate::oracle;
use crate::order::protocol::Decision;
use crate::order::protocol::MakerMessage;
use crate::order::protocol::TakerMessage;
use crate::order::PROTOCOL_NAME;
use crate::process_manager;
use crate::projection;
use crate::setup_contract;
use crate::wallet;
use crate::wire::SetupMsg;
use anyhow::bail;
use anyhow::Context;
use async_trait::async_trait;
use asynchronous_codec::Framed;
use asynchronous_codec::JsonCodec;
use futures::future;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::PeerId;
use maia_core::secp256k1_zkp::schnorrsig;
use model::olivia::BitMexPriceEventId;
use model::Cfd;
use model::Contracts;
use model::FundingRate;
use model::Identity;
use model::Leverage;
use model::OpeningFee;
use model::OrderId;
use model::Position;
use model::Price;
use model::Role;
use model::TxFeeRate;
use model::Usd;
use rust_decimal::Decimal;
use time::Duration;
use xtra::prelude::MessageChannel;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::Actor as _;
use xtra_libp2p::Endpoint;
use xtra_libp2p::OpenSubstream;
use xtra_productivity::xtra_productivity;
use xtras::spawner;
use xtras::spawner::SpawnFallible;
use xtras::SendAsyncSafe;

pub struct Actor {
    endpoint: xtra::Address<Endpoint>,
    executor: command::Executor,
    oracle_pk: schnorrsig::PublicKey,
    get_announcement: Box<dyn MessageChannel<oracle::GetAnnouncement>>,
    build_party_params: Box<dyn MessageChannel<wallet::BuildPartyParams> + 'static>,
    sign: Box<dyn MessageChannel<wallet::Sign> + 'static>,
    projection: xtra::Address<projection::Actor>,
    n_payouts: usize,
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
        endpoint: xtra::Address<Endpoint>,
    ) -> Self {
        let spawner = spawner::Actor::new().create(None).spawn_global();

        Self {
            endpoint,
            executor: command::Executor::new(db.clone(), process_manager),
            oracle_pk,
            get_announcement: get_announcement.clone_channel(),
            build_party_params: build_party_params.clone_channel(),
            sign: sign.clone_channel(),
            projection,
            n_payouts,
            db,
            spawner,
        }
    }
}

#[xtra_productivity]
impl Actor {
    pub async fn handle(&mut self, msg: PlaceOrder) {
        let id = msg.id;

        let task = {
            let build_party_params = self.build_party_params.clone_channel();
            let sign = self.sign.clone_channel();
            let get_announcement = self.get_announcement.clone_channel();
            let endpoint = self.endpoint.clone();
            let executor = self.executor.clone();
            let db = self.db.clone();
            let oracle_pk = self.oracle_pk;
            let n_payouts = self.n_payouts;
            let projection = self.projection.clone();
            async move {
                tracing::info!(order = ?msg, "Placing order");

                let PlaceOrder {
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
                    maker_peer_id: maker,
                } = msg;

                let cfd = Cfd::new(
                    id,
                    position.counter_position(),
                    opening_price,
                    leverage,
                    settlement_interval,
                    Role::Taker,
                    Usd::new(Decimal::from(u64::from(contracts))),
                    // Completely irrelevant when using libp2p
                    Identity::new(x25519_dalek::PublicKey::from(
                        *b"hello world, oh what a beautiful",
                    )),
                    Some(maker.into()),
                    opening_fee,
                    funding_rate,
                    tx_fee_rate,
                );

                // TODO: If this fails we shouldn't try to append
                // `ContractSetupFailed` to the nonexistent CFD
                db.insert_cfd(&cfd).await?;

                projection.send(projection::CfdChanged(cfd.id())).await?;

                let stream = endpoint
                    .send(OpenSubstream::single_protocol(maker, PROTOCOL_NAME))
                    .await??;

                let mut framed =
                    Framed::new(stream, JsonCodec::<TakerMessage, MakerMessage>::new());

                framed
                    .send(TakerMessage::PlaceOrder {
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
                    })
                    .await?;

                match framed.next().await.context("Stream terminated")?? {
                    MakerMessage::Decision(Decision::Accept) => {
                        tracing::info!(order_id = %msg.id, peer = %maker, "Order accepted");
                    }
                    MakerMessage::Decision(Decision::Reject) => {
                        tracing::info!(order_id = %msg.id, peer = %maker, "Order rejected");

                        executor
                            .execute(id, |cfd| {
                                cfd.reject_contract_setup(anyhow::anyhow!("Unknown"))
                            })
                            .await?;

                        return anyhow::Ok(());
                    }
                    MakerMessage::ContractSetupMsg(_) => bail!("Unexpected message"),
                };

                let (setup_params, position) = executor
                    .execute(id, |cfd| cfd.start_contract_setup())
                    .await?;

                let (sink, stream) = framed.split();

                let announcement = get_announcement
                    .send(oracle::GetAnnouncement(oracle_event_id))
                    .await??;

                let dlc = setup_contract::new(
                    sink.with(|msg| future::ok(TakerMessage::ContractSetupMsg(Box::new(msg)))),
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
                    Role::Taker,
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
    }
}

#[derive(Debug)]
pub(crate) struct PlaceOrder {
    id: OrderId,
    contracts: Contracts,
    leverage: Leverage,
    position: Position,
    opening_price: Price,
    settlement_interval: Duration,
    opening_fee: OpeningFee,
    funding_rate: FundingRate,
    tx_fee_rate: TxFeeRate,
    oracle_event_id: BitMexPriceEventId,
    maker_peer_id: PeerId,
}

impl PlaceOrder {
    pub(crate) fn new(
        id: OrderId,
        (contracts, leverage, position): (Contracts, Leverage, Position),
        opening_price: Price,
        settlement_interval: Duration,
        (opening_fee, funding_rate, tx_fee_rate): (OpeningFee, FundingRate, TxFeeRate),
        oracle_event_id: BitMexPriceEventId,
        maker_peer_id: PeerId,
    ) -> Self {
        Self {
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
            maker_peer_id,
        }
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
