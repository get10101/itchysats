use crate::command;
use crate::oracle;
use crate::oracle::NoAnnouncement;
use crate::order::contract_setup;
use crate::order::protocol::Decision;
use crate::order::protocol::MakerMessage;
use crate::order::protocol::SetupMsg;
use crate::order::protocol::TakerMessage;
use crate::order::PROTOCOL;
use crate::process_manager;
use crate::projection;
use crate::wallet;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use asynchronous_codec::Framed;
use asynchronous_codec::JsonCodec;
use bdk::bitcoin::psbt::PartiallySignedTransaction;
use bdk::bitcoin::XOnlyPublicKey;
use futures::future;
use futures::SinkExt;
use futures::StreamExt;
use libp2p_core::PeerId;
use maia_core::PartyParams;
use model::olivia;
use model::Cfd;
use model::Contracts;
use model::Identity;
use model::Leverage;
use model::Offer;
use model::OrderId;
use model::Role;
use xtra::prelude::MessageChannel;
use xtra_libp2p::Endpoint;
use xtra_libp2p::OpenSubstream;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    endpoint: xtra::Address<Endpoint>,
    executor: command::Executor,
    oracle_pk: XOnlyPublicKey,
    get_announcement:
        MessageChannel<oracle::GetAnnouncements, Result<Vec<olivia::Announcement>, NoAnnouncement>>,
    build_party_params: MessageChannel<wallet::BuildPartyParams, Result<PartyParams>>,
    sign: MessageChannel<wallet::Sign, Result<PartiallySignedTransaction>>,
    projection: xtra::Address<projection::Actor>,
    n_payouts: usize,
    db: sqlite_db::Connection,
}

impl Actor {
    pub fn new(
        n_payouts: usize,
        oracle_pk: XOnlyPublicKey,
        get_announcement: MessageChannel<
            oracle::GetAnnouncements,
            Result<Vec<olivia::Announcement>, NoAnnouncement>,
        >,
        (db, process_manager): (sqlite_db::Connection, xtra::Address<process_manager::Actor>),
        (build_party_params, sign): (
            MessageChannel<wallet::BuildPartyParams, Result<PartyParams>>,
            MessageChannel<wallet::Sign, Result<PartiallySignedTransaction>>,
        ),
        projection: xtra::Address<projection::Actor>,
        endpoint: xtra::Address<Endpoint>,
    ) -> Self {
        Self {
            endpoint,
            executor: command::Executor::new(db.clone(), process_manager),
            oracle_pk,
            get_announcement,
            build_party_params,
            sign,
            projection,
            n_payouts,
            db,
        }
    }
}

#[xtra_productivity]
impl Actor {
    pub async fn handle(&mut self, msg: PlaceOrder, ctx: &mut xtra::Context<Self>) {
        let id = msg.order_id;

        let task = {
            let build_party_params = self.build_party_params.clone();
            let sign = self.sign.clone();
            let get_announcement = self.get_announcement.clone();
            let endpoint = self.endpoint.clone();
            let executor = self.executor.clone();
            let db = self.db.clone();
            let oracle_pk = self.oracle_pk;
            let n_payouts = self.n_payouts;
            let projection = self.projection.clone();
            async move {
                tracing::info!(order = ?msg, "Placing order");

                let PlaceOrder {
                    order_id,
                    quantity,
                    leverage,
                    offer,
                    maker_identity,
                    maker_peer_id,
                } = msg;

                let oracle_event_id = offer.oracle_event_id;
                let cfd = Cfd::from_order(
                    order_id,
                    &offer,
                    quantity,
                    maker_identity,
                    Some(maker_peer_id.into()),
                    Role::Taker,
                    leverage,
                );

                // If this fails we shouldn't try to append
                // `ContractSetupFailed` to the nonexistent CFD
                if let Err(e) = db.insert_cfd(&cfd).await {
                    tracing::error!("Inserting new cfd failed: {e:#}");
                    return anyhow::Ok(());
                };

                projection.send(projection::CfdChanged(cfd.id())).await?;

                let stream = endpoint
                    .send(OpenSubstream::single_protocol(maker_peer_id, PROTOCOL))
                    .await
                    .context("Endpoint is disconnected")?
                    .context("No connection to peer")?
                    .await
                    .context("Failed to open substream")?;

                let mut framed =
                    Framed::new(stream, JsonCodec::<TakerMessage, MakerMessage>::new());

                framed
                    .send(TakerMessage::PlaceOrder {
                        id: order_id,
                        offer: Box::new(offer),
                        quantity,
                        leverage,
                    })
                    .await?;

                match framed.next().await.context("Stream terminated")?? {
                    MakerMessage::Decision(Decision::Accept) => {
                        tracing::info!(order_id = %msg.order_id, %maker_peer_id, "Order accepted");
                    }
                    MakerMessage::Decision(Decision::Reject) => {
                        tracing::info!(order_id = %msg.order_id, %maker_peer_id, "Order rejected");

                        executor
                            .execute(order_id, |cfd| {
                                cfd.reject_contract_setup(anyhow::anyhow!("Unknown"))
                            })
                            .await?;

                        return anyhow::Ok(());
                    }
                    MakerMessage::ContractSetupMsg(_) => bail!("Unexpected message"),
                };

                let (setup_params, position) = executor
                    .execute(order_id, |cfd| cfd.start_contract_setup())
                    .await?;

                let (sink, stream) = framed.split();

                let announcement = get_announcement
                    .send(oracle::GetAnnouncements(vec![oracle_event_id]))
                    .await??;

                let dlc = contract_setup::new(
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
                    .execute(order_id, |cfd| cfd.complete_contract_setup(dlc))
                    .await
                {
                    tracing::error!(%order_id, "Failed to execute contract_setup_completed: {e:#}");
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

        let address = ctx.address().expect("we are alive");
        tokio_extras::spawn_fallible(&address, task, err_handler);
    }
}

#[derive(Debug)]
pub(crate) struct PlaceOrder {
    order_id: OrderId,
    offer: Offer,
    quantity: Contracts,
    leverage: Leverage,
    maker_peer_id: PeerId,
    maker_identity: Identity,
}

impl PlaceOrder {
    pub(crate) fn new(
        order_id: OrderId,
        offer: Offer,
        (quantity, leverage): (Contracts, Leverage),
        maker_peer_id: PeerId,
        maker_identity: Identity,
    ) -> Self {
        Self {
            order_id,
            offer,
            quantity,
            leverage,
            maker_peer_id,
            maker_identity,
        }
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
