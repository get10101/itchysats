use crate::cfd_actors::insert_cfd_and_update_feed;
use crate::collab_settlement_maker;
use crate::command;
use crate::future_ext::FutureExt;
use crate::maker_inc_connections;
use crate::oracle;
use crate::process_manager;
use crate::projection;
use crate::rollover_maker;
use crate::setup_maker;
use crate::wallet;
use crate::wire;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::schnorrsig;
use model::olivia::BitMexPriceEventId;
use model::pick_single_offer;
use model::Cfd;
use model::FundingRate;
use model::Identity;
use model::MakerOffers;
use model::OpeningFee;
use model::Order;
use model::OrderId;
use model::Origin;
use model::Position;
use model::Price;
use model::Role;
use model::SettlementProposal;
use model::Timestamp;
use model::TxFeeRate;
use model::Usd;
use std::collections::HashSet;
use time::Duration;
use tokio_tasks::Tasks;
use xtra::Actor as _;
use xtra_productivity::xtra_productivity;
use xtras::AddressMap;
use xtras::SendAsyncSafe;

const HANDLE_ACCEPT_CONTRACT_SETUP_MESSAGE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(10);
const HANDLE_ACCEPT_ROLLOVER_MESSAGE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(10);
const HANDLE_ACCEPT_SETTLEMENT_MESSAGE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(120);

#[derive(Clone, Copy)]
pub struct NewOffers {
    pub params: OfferParams,
}

#[derive(Clone, Copy)]
pub struct AcceptOrder {
    pub order_id: OrderId,
}

#[derive(Clone, Copy)]
pub struct RejectOrder {
    pub order_id: OrderId,
}

#[derive(Clone, Copy)]
pub struct AcceptSettlement {
    pub order_id: OrderId,
}

#[derive(Clone, Copy)]
pub struct RejectSettlement {
    pub order_id: OrderId,
}

#[derive(Clone, Copy)]
pub struct AcceptRollover {
    pub order_id: OrderId,
}

#[derive(Clone, Copy)]
pub struct RejectRollover {
    pub order_id: OrderId,
}

#[derive(Clone, Copy)]
pub struct TakerConnected {
    pub id: Identity,
}

#[derive(Clone, Copy)]
pub struct TakerDisconnected {
    pub id: Identity,
}

#[derive(Clone, Copy)]
pub struct OfferParams {
    pub price_long: Option<Price>,
    pub price_short: Option<Price>,
    pub min_quantity: Usd,
    pub max_quantity: Usd,
    pub tx_fee_rate: TxFeeRate,
    pub funding_rate: FundingRate,
    pub opening_fee: OpeningFee,
}

impl OfferParams {
    fn pick_oracle_event_id(settlement_interval: Duration) -> BitMexPriceEventId {
        oracle::next_announcement_after(time::OffsetDateTime::now_utc() + settlement_interval)
    }

    pub fn create_long_order(&self, settlement_interval: Duration) -> Option<Order> {
        self.price_long.map(|price_long| {
            Order::new(
                Position::Long,
                price_long,
                self.min_quantity,
                self.max_quantity,
                Origin::Ours,
                Self::pick_oracle_event_id(settlement_interval),
                settlement_interval,
                self.tx_fee_rate,
                self.funding_rate,
                self.opening_fee,
            )
        })
    }

    pub fn create_short_order(&self, settlement_interval: Duration) -> Option<Order> {
        self.price_short.map(|price_short| {
            Order::new(
                Position::Short,
                price_short,
                self.min_quantity,
                self.max_quantity,
                Origin::Ours,
                Self::pick_oracle_event_id(settlement_interval),
                settlement_interval,
                self.tx_fee_rate,
                self.funding_rate,
                self.opening_fee,
            )
        })
    }
}

fn create_maker_offers(offer_params: OfferParams, settlement_interval: Duration) -> MakerOffers {
    MakerOffers {
        long: offer_params.create_long_order(settlement_interval),
        short: offer_params.create_short_order(settlement_interval),
        tx_fee_rate: offer_params.tx_fee_rate,
        funding_rate: offer_params.funding_rate,
    }
}

pub struct FromTaker {
    pub taker_id: Identity,
    pub msg: wire::TakerToMaker,
}

/// Proposed rollover
#[derive(Debug, Clone, PartialEq)]
struct RolloverProposal {
    pub order_id: OrderId,
    pub timestamp: Timestamp,
}

pub struct Actor<O, T, W> {
    db: sqlx::PgPool,
    wallet: xtra::Address<W>,
    settlement_interval: Duration,
    oracle_pk: schnorrsig::PublicKey,
    projection: xtra::Address<projection::Actor>,
    process_manager: xtra::Address<process_manager::Actor>,
    executor: command::Executor,
    rollover_actors: AddressMap<OrderId, rollover_maker::Actor>,
    takers: xtra::Address<T>,
    current_offers: Option<MakerOffers>,
    setup_actors: AddressMap<OrderId, setup_maker::Actor>,
    settlement_actors: AddressMap<OrderId, collab_settlement_maker::Actor>,
    oracle: xtra::Address<O>,
    connected_takers: HashSet<Identity>,
    n_payouts: usize,
    tasks: Tasks,
}

impl<O, T, W> Actor<O, T, W> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: sqlx::PgPool,
        wallet: xtra::Address<W>,
        settlement_interval: Duration,
        oracle_pk: schnorrsig::PublicKey,
        projection: xtra::Address<projection::Actor>,
        process_manager: xtra::Address<process_manager::Actor>,
        takers: xtra::Address<T>,
        oracle: xtra::Address<O>,
        n_payouts: usize,
    ) -> Self {
        Self {
            db: db.clone(),
            wallet,
            settlement_interval,
            oracle_pk,
            projection,
            process_manager: process_manager.clone(),
            executor: command::Executor::new(db, process_manager),
            rollover_actors: AddressMap::default(),
            takers,
            current_offers: None,
            setup_actors: AddressMap::default(),
            oracle,
            n_payouts,
            connected_takers: HashSet::new(),
            settlement_actors: AddressMap::default(),
            tasks: Tasks::default(),
        }
    }

    async fn update_connected_takers(&mut self) -> Result<()> {
        self.projection
            .send(projection::Update(
                self.connected_takers
                    .clone()
                    .into_iter()
                    .collect::<Vec<Identity>>(),
            ))
            .await?;
        Ok(())
    }
}

impl<O, T, W> Actor<O, T, W>
where
    T: xtra::Handler<maker_inc_connections::TakerMessage>,
{
    async fn handle_taker_connected(&mut self, taker_id: Identity) -> Result<()> {
        self.takers
            .send_async_safe(maker_inc_connections::TakerMessage {
                taker_id,
                msg: wire::MakerToTaker::CurrentOffers(self.current_offers),
            })
            .await?;

        if !self.connected_takers.insert(taker_id) {
            tracing::warn!("Taker already connected: {:?}", &taker_id);
        }
        self.update_connected_takers().await?;
        Ok(())
    }

    async fn handle_taker_disconnected(&mut self, taker_id: Identity) -> Result<()> {
        if !self.connected_takers.remove(&taker_id) {
            tracing::warn!("Removed unknown taker: {:?}", &taker_id);
        }
        self.update_connected_takers().await?;
        Ok(())
    }
}

impl<O, T, W> Actor<O, T, W>
where
    O: xtra::Handler<oracle::GetAnnouncement> + xtra::Handler<oracle::MonitorAttestation>,
    T: xtra::Handler<maker_inc_connections::TakerMessage>
        + xtra::Handler<maker_inc_connections::RegisterRollover>,
    W: 'static,
{
    async fn handle_propose_rollover(
        &mut self,
        RolloverProposal { order_id, .. }: RolloverProposal,
        taker_id: Identity,
    ) -> Result<()> {
        tracing::info!(%order_id, %taker_id,  "Received rollover proposal from taker");

        let rollover_actor_addr = rollover_maker::Actor::new(
            order_id,
            self.n_payouts,
            &self.takers,
            taker_id,
            self.oracle_pk,
            &self.oracle,
            self.process_manager.clone(),
            &self.takers,
            self.db.clone(),
        )
        .create(None)
        .spawn(&mut self.tasks);

        self.rollover_actors.insert(order_id, rollover_actor_addr);

        Ok(())
    }
}

impl<O, T, W> Actor<O, T, W>
where
    O: xtra::Handler<oracle::GetAnnouncement> + xtra::Handler<oracle::MonitorAttestation>,
    T: xtra::Handler<maker_inc_connections::ConfirmOrder>
        + xtra::Handler<maker_inc_connections::TakerMessage>
        + xtra::Handler<maker_inc_connections::BroadcastOffers>,
    W: xtra::Handler<wallet::Sign> + xtra::Handler<wallet::BuildPartyParams>,
{
    async fn handle_take_order(
        &mut self,
        taker_id: Identity,
        order_id: OrderId,
        quantity: Usd,
    ) -> Result<()> {
        tracing::debug!(%taker_id, %quantity, %order_id, "Taker wants to take an order");

        let disconnected = self
            .setup_actors
            .get_disconnected(order_id)
            .with_context(|| {
                format!("Contract setup for order {order_id} is already in progress")
            })?;

        let mut conn = self.db.acquire().await?;

        // 1. Validate if order is still valid
        let current_order = self
            .current_offers
            .and_then(|offers| offers.take_offer_by_order_id(order_id));

        let current_order = if let Some(current_order) = current_order {
            current_order
        } else {
            // An outdated order on the taker side does not require any state change on the
            // maker. notifying the taker with a specific message should be sufficient.
            // Since this is a scenario that we should rarely see we log
            // a warning to be sure we don't trigger this code path frequently.
            tracing::warn!("Taker tried to take order with outdated id {order_id}");

            self.takers
                .send(maker_inc_connections::TakerMessage {
                    taker_id,
                    msg: wire::MakerToTaker::InvalidOrderId(order_id),
                })
                .await??;

            return Ok(());
        };

        let cfd = Cfd::from_order(current_order, quantity, taker_id, Role::Maker);

        // 2. Replicate the orders in the offers with new ones to allow other takers to use
        // the same offer
        if let Some(offers) = self.current_offers {
            self.current_offers = Some(offers.replicate());
        }

        self.takers
            .send_async_safe(maker_inc_connections::BroadcastOffers(self.current_offers))
            .await?;

        let order = pick_single_offer(self.current_offers);
        self.projection.send(projection::Update(order)).await?;
        insert_cfd_and_update_feed(&cfd, &mut conn, &self.projection).await?;

        // 4. Try to get the oracle announcement, if that fails we should exit prior to changing any
        // state
        let announcement = self
            .oracle
            .send(oracle::GetAnnouncement(current_order.oracle_event_id))
            .await??;

        // 5. Start up contract setup actor
        let addr = setup_maker::Actor::new(
            self.db.clone(),
            self.process_manager.clone(),
            (current_order, cfd.quantity(), self.n_payouts),
            (self.oracle_pk, announcement),
            &self.wallet,
            &self.wallet,
            (&self.takers, &self.takers, taker_id),
        )
        .create(None)
        .spawn(&mut self.tasks);

        disconnected.insert(addr);

        Ok(())
    }
}

#[xtra_productivity]
impl<O, T, W> Actor<O, T, W> {
    async fn handle_accept_order(&mut self, msg: AcceptOrder) -> Result<()> {
        let AcceptOrder { order_id } = msg;

        tracing::debug!(%order_id, "Maker accepts order");

        if let Err(error) = self
            .setup_actors
            .send(&order_id, setup_maker::Accepted)
            .timeout(HANDLE_ACCEPT_CONTRACT_SETUP_MESSAGE_TIMEOUT)
            .await
        {
            self.executor
                .execute(order_id, |cfd| Ok(cfd.fail_contract_setup(anyhow!(error))))
                .await?;

            bail!("Accept failed: No active contract setup for order {order_id}")
        }

        Ok(())
    }

    async fn handle_reject_order(&mut self, msg: RejectOrder) -> Result<()> {
        let RejectOrder { order_id } = msg;

        tracing::debug!(%order_id, "Maker rejects order");

        if let Err(error) = self
            .setup_actors
            .send(&order_id, setup_maker::Rejected)
            .timeout(HANDLE_ACCEPT_CONTRACT_SETUP_MESSAGE_TIMEOUT)
            .await
        {
            self.executor
                .execute(order_id, |cfd| Ok(cfd.fail_contract_setup(anyhow!(error))))
                .await?;

            bail!("Reject failed: No active contract setup for order {order_id}")
        }

        Ok(())
    }

    async fn handle_accept_settlement(&mut self, msg: AcceptSettlement) -> Result<()> {
        let AcceptSettlement { order_id } = msg;

        if let Err(error) = self
            .settlement_actors
            .send(&order_id, collab_settlement_maker::Accepted)
            .timeout(HANDLE_ACCEPT_SETTLEMENT_MESSAGE_TIMEOUT)
            .await
        {
            self.executor
                .execute(order_id, |cfd| {
                    Ok(cfd.fail_collaborative_settlement(anyhow!(error)))
                })
                .await?;

            bail!("Accept failed: No settlement in progress for order {order_id}")
        }

        Ok(())
    }

    async fn handle_reject_settlement(&mut self, msg: RejectSettlement) -> Result<()> {
        let RejectSettlement { order_id } = msg;

        if let Err(error) = self
            .settlement_actors
            .send(&order_id, collab_settlement_maker::Rejected)
            .timeout(HANDLE_ACCEPT_SETTLEMENT_MESSAGE_TIMEOUT)
            .await
        {
            self.executor
                .execute(order_id, |cfd| {
                    Ok(cfd.fail_collaborative_settlement(anyhow!(error)))
                })
                .await?;

            bail!("Reject failed: No settlement in progress for order {order_id}")
        }

        Ok(())
    }

    async fn handle_accept_rollover(&mut self, msg: AcceptRollover) -> Result<()> {
        let current_offers = self
            .current_offers
            .as_ref()
            .context("Cannot accept rollover without current offer, as we need up-to-date fees")?;

        let order_id = msg.order_id;

        if let Err(error) = self
            .rollover_actors
            .send(
                &order_id,
                rollover_maker::AcceptRollover {
                    tx_fee_rate: current_offers.tx_fee_rate,
                    funding_rate: current_offers.funding_rate,
                },
            )
            .timeout(HANDLE_ACCEPT_ROLLOVER_MESSAGE_TIMEOUT)
            .await
        {
            self.executor
                .execute(order_id, |cfd| Ok(cfd.fail_rollover(anyhow!(error))))
                .await?;

            bail!("Accept failed: No active rollover for order {order_id}")
        }

        Ok(())
    }

    async fn handle_reject_rollover(&mut self, msg: RejectRollover) -> Result<()> {
        let order_id = msg.order_id;

        if let Err(error) = self
            .rollover_actors
            .send(&order_id, rollover_maker::RejectRollover)
            .timeout(HANDLE_ACCEPT_ROLLOVER_MESSAGE_TIMEOUT)
            .await
        {
            self.executor
                .execute(order_id, |cfd| Ok(cfd.fail_rollover(anyhow!(error))))
                .await?;

            bail!("Reject failed: No active rollover for order {order_id}")
        }

        Ok(())
    }
}

impl<O, T, W> Actor<O, T, W>
where
    O: xtra::Handler<oracle::MonitorAttestation>,
    T: xtra::Handler<maker_inc_connections::settlement::Response>,
    W: 'static + Send,
{
    async fn handle_propose_settlement(
        &mut self,
        taker_id: Identity,
        proposal: SettlementProposal,
    ) -> Result<()> {
        let order_id = proposal.order_id;

        let disconnected = self
            .settlement_actors
            .get_disconnected(order_id)
            .with_context(|| format!("Settlement for order {order_id} is already in progress",))?;

        let addr = collab_settlement_maker::Actor::new(
            proposal,
            taker_id,
            &self.takers,
            self.process_manager.clone(),
            self.db.clone(),
            self.n_payouts,
        )
        .create(None)
        .spawn(&mut self.tasks);

        disconnected.insert(addr);

        Ok(())
    }
}

#[xtra_productivity]
impl<O, T, W> Actor<O, T, W>
where
    O: xtra::Handler<oracle::GetAnnouncement> + xtra::Handler<oracle::MonitorAttestation>,
    T: xtra::Handler<maker_inc_connections::ConfirmOrder>
        + xtra::Handler<maker_inc_connections::TakerMessage>
        + xtra::Handler<maker_inc_connections::BroadcastOffers>
        + xtra::Handler<maker_inc_connections::settlement::Response>
        + xtra::Handler<maker_inc_connections::RegisterRollover>,
    W: xtra::Handler<wallet::Sign> + xtra::Handler<wallet::BuildPartyParams>,
{
    async fn handle_new_order(&mut self, msg: OfferParams) -> Result<()> {
        // 1. Update actor state to current order
        self.current_offers
            .replace(create_maker_offers(msg, self.settlement_interval));

        let order = pick_single_offer(self.current_offers);

        // 2. Notify UI via feed
        self.projection.send(projection::Update(order)).await?;

        // 3. Inform connected takers
        self.takers
            .send_async_safe(maker_inc_connections::BroadcastOffers(self.current_offers))
            .await?;

        Ok(())
    }

    async fn handle(&mut self, msg: TakerConnected) -> Result<()> {
        self.handle_taker_connected(msg.id).await
    }

    async fn handle(&mut self, msg: TakerDisconnected) -> Result<()> {
        self.handle_taker_disconnected(msg.id).await
    }

    async fn handle(&mut self, FromTaker { taker_id, msg }: FromTaker) {
        match msg {
            wire::TakerToMaker::TakeOrder { order_id, quantity } => {
                if let Err(e) = self.handle_take_order(taker_id, order_id, quantity).await {
                    tracing::error!("Error when handling order take request: {:#}", e)
                }
            }
            wire::TakerToMaker::Settlement {
                order_id,
                msg:
                    wire::taker_to_maker::Settlement::Propose {
                        timestamp,
                        taker,
                        maker,
                        price,
                    },
            } => {
                if let Err(e) = self
                    .handle_propose_settlement(
                        taker_id,
                        SettlementProposal {
                            order_id,
                            timestamp,
                            taker,
                            maker,
                            price,
                        },
                    )
                    .await
                {
                    tracing::warn!(%order_id, "Failed to handle settlement proposal: {:#}", e);
                }
            }
            wire::TakerToMaker::Settlement {
                msg: wire::taker_to_maker::Settlement::Initiate { .. },
                ..
            } => {
                unreachable!("Handled within `collab_settlement_maker::Actor");
            }
            wire::TakerToMaker::ProposeRollover {
                order_id,
                timestamp,
            } => {
                if let Err(e) = self
                    .handle_propose_rollover(
                        RolloverProposal {
                            order_id,
                            timestamp,
                        },
                        taker_id,
                    )
                    .await
                {
                    tracing::warn!("Failed to handle rollover proposal: {:#}", e);
                }
            }
            wire::TakerToMaker::RolloverProtocol { .. } => {
                unreachable!("This kind of message should be sent to the rollover_maker::Actor`")
            }
            wire::TakerToMaker::Protocol { .. } => {
                unreachable!("This kind of message should be sent to the `setup_maker::Actor`")
            }
            wire::TakerToMaker::Hello(_) => {
                unreachable!("The Hello message is not sent to the cfd actor")
            }
        }
    }
}

#[async_trait]
impl<O: Send + 'static, T: Send + 'static, W: Send + 'static> xtra::Actor for Actor<O, T, W> {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
