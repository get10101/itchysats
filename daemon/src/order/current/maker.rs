use crate::command;
use crate::oracle;
use crate::oracle::NoAnnouncement;
use crate::order::current::contract_setup;
use crate::order::current::protocol;
use crate::order::current::protocol::MakerMessage;
use crate::order::current::protocol::SetupMsg;
use crate::order::current::protocol::TakerMessage;
use crate::process_manager;
use crate::projection;
use crate::wallet;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use asynchronous_codec::Framed;
use asynchronous_codec::JsonCodec;
use bdk::bitcoin::psbt::PartiallySignedTransaction;
use bdk::bitcoin::XOnlyPublicKey;
use futures::channel::oneshot;
use futures::future;
use futures::SinkExt;
use futures::StreamExt;
use maia_core::PartyParams;
use model::olivia;
use model::Cfd;
use model::Identity;
use model::OfferId;
use model::OrderId;
use model::Role;
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;
use tokio_extras::FutureExt;
use tracing::instrument;
use xtra::prelude::MessageChannel;
use xtra_libp2p::NewInboundSubstream;
use xtra_libp2p::Substream;
use xtra_productivity::xtra_productivity;
use xtras::SendAsyncSafe;

const ORDER_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Actor {
    executor: command::Executor,
    oracle_pk: XOnlyPublicKey,
    get_announcement:
        MessageChannel<oracle::GetAnnouncements, Result<Vec<olivia::Announcement>, NoAnnouncement>>,
    build_party_params: MessageChannel<wallet::BuildPartyParams, Result<PartyParams>>,
    sign: MessageChannel<wallet::Sign, Result<PartiallySignedTransaction>>,
    projection: xtra::Address<projection::Actor>,
    n_payouts: usize,
    decision_senders: HashMap<OrderId, oneshot::Sender<protocol::Decision>>,
    db: sqlite_db::Connection,
    latest_offers: MessageChannel<offer::maker::GetLatestOffers, Vec<model::Offer>>,
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
        latest_offers: MessageChannel<offer::maker::GetLatestOffers, Vec<model::Offer>>,
    ) -> Self {
        Self {
            executor: command::Executor::new(db.clone(), process_manager),
            oracle_pk,
            get_announcement,
            build_party_params,
            sign,
            projection,
            n_payouts,
            decision_senders: HashMap::default(),
            db,
            latest_offers,
        }
    }

    #[instrument(skip(self), err)]
    async fn receive_order(
        &mut self,
        framed: &mut Framed<Substream, JsonCodec<MakerMessage, TakerMessage>>,
    ) -> Result<TakerMessage> {
        let order = framed
            .next()
            .timeout(ORDER_TIMEOUT, || tracing::debug_span!("receive order"))
            .await
            .context("Timeout when waiting for order")?
            .context("Stream terminated")?
            .context("Unable to decode order")?;

        Ok(order)
    }

    #[instrument(skip(self))]
    async fn pick_offer(&self, offer_id: OfferId) -> Result<model::Offer> {
        let latest_offers = self
            .latest_offers
            .send(offer::maker::GetLatestOffers)
            .await
            .context("Failed to retrieve latest offer from offers actor")?;

        let offer = latest_offers
            .iter()
            .find(|offer| offer.id == offer_id)
            .with_context(|| format!("Offer with id {offer_id} not found in current offers"))?
            .clone();

        Ok(offer)
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle(&mut self, msg: NewInboundSubstream, ctx: &mut xtra::Context<Self>) {
        let NewInboundSubstream { peer_id, stream } = msg;

        let mut framed = Framed::new(stream, JsonCodec::<MakerMessage, TakerMessage>::new());

        let order = match self.receive_order(&mut framed).await {
            Ok(order) => order,
            Err(e) => {
                tracing::error!("Failed to receive order from taker: {e:#}");
                return;
            }
        };

        let (order_id, offer_id, quantity, leverage) = match order {
            TakerMessage::PlaceOrder {
                id,
                offer,
                quantity,
                leverage,
            } => (id, offer.id, quantity, leverage),
            TakerMessage::ContractSetupMsg(_) => {
                tracing::error!("Unexpected message");
                return;
            }
        };

        tracing::info!(%peer_id, %quantity, %order_id, %offer_id, "Taker wants to place an order");

        // Reject the order if the offer cannot be found in the latest offers
        let offer = match self.pick_offer(offer_id).await {
            Ok(offer) => offer,
            Err(e) => {
                tracing::warn!(
                    %peer_id,
                    "Rejecting taker order because unable to pick offer: {e:#}"
                );

                let future = async move {
                    framed
                        .send(MakerMessage::Decision(protocol::Decision::Reject))
                        .await?;

                    anyhow::Ok(())
                };

                tokio_extras::spawn_fallible(
                    &ctx.address().expect("self to be alive"),
                    future,
                    move |e| async move {
                        tracing::debug!(%peer_id, "Failed to send reject order message: {e}");
                    },
                );

                return;
            }
        };

        let oracle_event_id = offer.oracle_event_id;

        let cfd = Cfd::from_order(
            order_id,
            &offer,
            quantity,
            Identity::new(x25519_dalek::PublicKey::from(
                *b"hello world, oh what a beautiful",
            )),
            Some(peer_id.into()),
            Role::Maker,
            leverage,
        );

        // If this fails we shouldn't try to append
        // `ContractSetupFailed` to the nonexistent CFD
        if let Err(e) = self.db.insert_cfd(&cfd).await {
            tracing::error!("Inserting new cfd failed: {e:#}");
            return;
        }

        if let Err(e) = self
            .projection
            .send_async_safe(projection::CfdChanged(cfd.id()))
            .await
        {
            tracing::error!(%order_id, "Failed to update projection with new cfd when handling order: {e:#}");
            return;
        }

        let (sender, receiver) = oneshot::channel();
        self.decision_senders.insert(order_id, sender);

        let task = {
            let build_party_params = self.build_party_params.clone();
            let sign = self.sign.clone();
            let get_announcement = self.get_announcement.clone();
            let executor = self.executor.clone();
            let oracle_pk = self.oracle_pk;
            let n_payouts = self.n_payouts;
            async move {
                match receiver.await? {
                    protocol::Decision::Accept => {
                        framed
                            .send(MakerMessage::Decision(protocol::Decision::Accept))
                            .await?;

                        tracing::info!(%peer_id, %quantity, %order_id, "Order accepted");
                    }
                    protocol::Decision::Reject => {
                        framed
                            .send(MakerMessage::Decision(protocol::Decision::Reject))
                            .await?;

                        tracing::info!(%peer_id, %quantity, %order_id, "Order rejected");

                        executor
                            .execute(order_id, |cfd| {
                                cfd.reject_contract_setup(anyhow::anyhow!("Unknown"))
                            })
                            .await?;

                        return anyhow::Ok(());
                    }
                }

                let (setup_params, position) = executor
                    .execute(order_id, |cfd| cfd.start_contract_setup())
                    .await?;

                let (sink, stream) = framed.split();

                let announcement = get_announcement
                    .send(oracle::GetAnnouncements(vec![oracle_event_id]))
                    .await??;

                let dlc = contract_setup::new(
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
                    .execute(order_id, |cfd| Ok(cfd.fail_contract_setup(e)))
                    .await
                {
                    tracing::error!(%order_id, "Failed to execute fail_contract_setup: {e:#}");
                }
            }
        };

        let address = ctx.address().expect("we are alive");
        tokio_extras::spawn_fallible(&address, task, err_handler);
    }

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
            Decision::Accept(_) => "Accept",
            Decision::Reject(_) => "Reject",
        };

        s.fmt(f)
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
