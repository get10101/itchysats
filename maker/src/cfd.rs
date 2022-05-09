use crate::collab_settlement;
use crate::connection;
use crate::contract_setup;
use crate::future_ext::FutureExt;
use crate::metrics::time_to_first_position;
use crate::rollover;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use daemon::command;
use daemon::db;
use daemon::oracle;
use daemon::process_manager;
use daemon::projection;
use daemon::wallet;
use daemon::wire;
use maia::secp256k1_zkp::schnorrsig;
use model::olivia::BitMexPriceEventId;
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
use model::RolloverVersion;
use model::SettlementProposal;
use model::Timestamp;
use model::TxFeeRate;
use model::Usd;
use std::collections::HashSet;
use time::Duration;
use tokio_tasks::Tasks;
use xtra::Actor as _;
use xtra_productivity::xtra_productivity;
use xtras::address_map::NotConnected;
use xtras::AddressMap;
use xtras::SendAsyncSafe;

const HANDLE_ACCEPT_CONTRACT_SETUP_MESSAGE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(10);

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
    pub funding_rate_long: FundingRate,
    pub funding_rate_short: FundingRate,
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
                self.funding_rate_long,
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
                self.funding_rate_short,
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
        funding_rate_long: offer_params.funding_rate_long,
        funding_rate_short: offer_params.funding_rate_short,
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
    db: db::Connection,
    wallet: xtra::Address<W>,
    settlement_interval: Duration,
    oracle_pk: schnorrsig::PublicKey,
    projection: xtra::Address<projection::Actor>,
    process_manager: xtra::Address<process_manager::Actor>,
    executor: command::Executor,
    rollover_actors: AddressMap<OrderId, rollover::Actor>,
    takers: xtra::Address<T>,
    current_offers: Option<MakerOffers>,
    setup_actors: AddressMap<OrderId, contract_setup::Actor>,
    settlement_actors: AddressMap<OrderId, collab_settlement::Actor>,
    oracle: xtra::Address<O>,
    time_to_first_position: xtra::Address<time_to_first_position::Actor>,
    connected_takers: HashSet<Identity>,
    n_payouts: usize,
    tasks: Tasks,
}

impl<O, T, W> Actor<O, T, W> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: db::Connection,
        wallet: xtra::Address<W>,
        settlement_interval: Duration,
        oracle_pk: schnorrsig::PublicKey,
        projection: xtra::Address<projection::Actor>,
        process_manager: xtra::Address<process_manager::Actor>,
        takers: xtra::Address<T>,
        oracle: xtra::Address<O>,
        time_to_first_position: xtra::Address<time_to_first_position::Actor>,
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
            time_to_first_position,
            n_payouts,
            connected_takers: HashSet::new(),
            settlement_actors: AddressMap::default(),
            tasks: Tasks::default(),
        }
    }

    async fn update_connected_takers(&mut self) -> Result<()> {
        self.projection
            .send_async_safe(projection::Update(
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
    T: xtra::Handler<connection::TakerMessage>,
{
    async fn handle_taker_connected(&mut self, taker_id: Identity) -> Result<()> {
        self.takers
            .send_async_safe(connection::TakerMessage {
                taker_id,
                msg: wire::MakerToTaker::CurrentOffers(self.current_offers),
            })
            .await?;

        if !self.connected_takers.insert(taker_id) {
            tracing::warn!("Taker already connected: {:?}", &taker_id);
        }
        self.update_connected_takers().await?;
        self.time_to_first_position
            .send_async_safe(time_to_first_position::Connected::new(taker_id))
            .await?;
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
    T: xtra::Handler<connection::TakerMessage> + xtra::Handler<connection::RegisterRollover>,
    W: 'static,
{
    async fn handle_propose_rollover(
        &mut self,
        RolloverProposal { order_id, .. }: RolloverProposal,
        taker_id: Identity,
        version: RolloverVersion,
    ) -> Result<()> {
        let rollover_actor_addr = rollover::Actor::new(
            order_id,
            self.n_payouts,
            &self.takers,
            taker_id,
            self.oracle_pk,
            &self.oracle,
            self.process_manager.clone(),
            &self.takers,
            self.db.clone(),
            version,
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
    T: xtra::Handler<connection::ConfirmOrder>
        + xtra::Handler<connection::TakerMessage>
        + xtra::Handler<connection::BroadcastOffers>,
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

        // 1. Validate if order is still valid
        let order_to_take = self
            .current_offers
            .and_then(|offers| offers.pick_order_to_take(order_id));

        let order_to_take = if let Some(order_to_take) = order_to_take {
            order_to_take
        } else {
            // An outdated order on the taker side does not require any state change on the
            // maker. notifying the taker with a specific message should be sufficient.
            // Since this is a scenario that we should rarely see we log
            // a warning to be sure we don't trigger this code path frequently.
            tracing::warn!("Taker tried to take order with outdated id {order_id}");

            self.takers
                .send(connection::TakerMessage {
                    taker_id,
                    msg: wire::MakerToTaker::InvalidOrderId(order_id),
                })
                .await??;

            return Ok(());
        };

        let cfd = Cfd::from_order(order_to_take, quantity, taker_id, Role::Maker);

        // 2. Replicate the orders in the offers with new ones to allow other takers to use
        // the same offer
        if let Some(offers) = self.current_offers {
            self.current_offers = Some(offers.replicate());
        }

        self.takers
            .send_async_safe(connection::BroadcastOffers(self.current_offers))
            .await?;

        self.projection
            .send(projection::Update(self.current_offers))
            .await?;

        self.db.insert_cfd(&cfd).await?;
        self.projection
            .send(projection::CfdChanged(cfd.id()))
            .await?;

        // 4. Try to get the oracle announcement, if that fails we should exit prior to changing any
        // state
        let announcement = self
            .oracle
            .send(oracle::GetAnnouncement(order_to_take.oracle_event_id))
            .await??;

        // 5. Start up contract setup actor
        let addr = contract_setup::Actor::new(
            self.db.clone(),
            self.process_manager.clone(),
            (order_to_take, cfd.quantity(), self.n_payouts),
            (self.oracle_pk, announcement),
            &self.wallet,
            &self.wallet,
            (&self.takers, &self.takers, taker_id),
            self.time_to_first_position.clone(),
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

        match self
            .setup_actors
            .send(&order_id, contract_setup::Accepted)
            .timeout(HANDLE_ACCEPT_CONTRACT_SETUP_MESSAGE_TIMEOUT)
            .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(NotConnected(e))) => {
                self.executor
                    .execute(order_id, |cfd| Ok(cfd.fail_contract_setup(anyhow!(e))))
                    .await?;

                bail!("Accept failed: No active contract setup for order {order_id}")
            }
            Err(elapsed) => {
                self.executor
                    .execute(
                        order_id,
                        |cfd| Ok(cfd.fail_contract_setup(anyhow!(elapsed))),
                    )
                    .await?;

                bail!("Accept failed: Contract setup stale for order {order_id}")
            }
        }
    }

    async fn handle_reject_order(&mut self, msg: RejectOrder) -> Result<()> {
        let RejectOrder { order_id } = msg;

        tracing::debug!(%order_id, "Maker rejects order");

        match self
            .setup_actors
            .send(&order_id, contract_setup::Rejected)
            .timeout(HANDLE_ACCEPT_CONTRACT_SETUP_MESSAGE_TIMEOUT)
            .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(NotConnected(e))) => {
                self.executor
                    .execute(order_id, |cfd| Ok(cfd.fail_contract_setup(anyhow!(e))))
                    .await?;

                bail!("Reject failed: No active contract setup for order {order_id}")
            }
            Err(elapsed) => {
                self.executor
                    .execute(
                        order_id,
                        |cfd| Ok(cfd.fail_contract_setup(anyhow!(elapsed))),
                    )
                    .await?;

                bail!("Reject failed: Contract setup stale for order {order_id}")
            }
        }
    }

    async fn handle_accept_settlement(&mut self, msg: AcceptSettlement) -> Result<()> {
        let AcceptSettlement { order_id } = msg;

        match self
            .settlement_actors
            .send_async(&order_id, collab_settlement::Accepted)
            .await
        {
            Ok(_) => Ok(()),
            Err(NotConnected(e)) => {
                self.executor
                    .execute(order_id, |cfd| {
                        Ok(cfd.fail_collaborative_settlement(anyhow!(e)))
                    })
                    .await?;

                bail!("Accept failed: No settlement in progress for order {order_id}")
            }
        }
    }

    async fn handle_reject_settlement(&mut self, msg: RejectSettlement) -> Result<()> {
        let RejectSettlement { order_id } = msg;

        match self
            .settlement_actors
            .send_async(&order_id, collab_settlement::Rejected)
            .await
        {
            Ok(_) => Ok(()),
            Err(NotConnected(e)) => {
                self.executor
                    .execute(order_id, |cfd| {
                        Ok(cfd.fail_collaborative_settlement(anyhow!(e)))
                    })
                    .await?;

                bail!("Reject failed: No settlement in progress for order {order_id}")
            }
        }
    }

    async fn handle_accept_rollover(&mut self, msg: AcceptRollover) -> Result<()> {
        let current_offers = self
            .current_offers
            .as_ref()
            .context("Cannot accept rollover without current offer, as we need up-to-date fees")?;

        let order_id = msg.order_id;

        match self
            .rollover_actors
            .send_async(
                &order_id,
                rollover::AcceptRollover {
                    tx_fee_rate: current_offers.tx_fee_rate,
                    long_funding_rate: current_offers.funding_rate_long,
                    short_funding_rate: current_offers.funding_rate_short,
                },
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(NotConnected(e)) => {
                self.executor
                    .execute(order_id, |cfd| Ok(cfd.fail_rollover(anyhow!(e))))
                    .await?;

                bail!("Accept failed: No active rollover for order {order_id}")
            }
        }
    }

    async fn handle_reject_rollover(&mut self, msg: RejectRollover) -> Result<()> {
        let order_id = msg.order_id;

        match self
            .rollover_actors
            .send_async(&order_id, rollover::RejectRollover)
            .await
        {
            Ok(_) => Ok(()),
            Err(NotConnected(e)) => {
                self.executor
                    .execute(order_id, |cfd| Ok(cfd.fail_rollover(anyhow!(e))))
                    .await?;

                bail!("Reject failed: No active rollover for order {order_id}")
            }
        }
    }
}

impl<O, T, W> Actor<O, T, W>
where
    O: xtra::Handler<oracle::MonitorAttestation>,
    T: xtra::Handler<connection::settlement::Response>,
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

        let addr = collab_settlement::Actor::new(
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
    T: xtra::Handler<connection::ConfirmOrder>
        + xtra::Handler<connection::TakerMessage>
        + xtra::Handler<connection::BroadcastOffers>
        + xtra::Handler<connection::settlement::Response>
        + xtra::Handler<connection::RegisterRollover>,
    W: xtra::Handler<wallet::Sign> + xtra::Handler<wallet::BuildPartyParams>,
{
    async fn handle_new_order(&mut self, msg: OfferParams) -> Result<()> {
        // 1. Update actor state to current order
        self.current_offers
            .replace(create_maker_offers(msg, self.settlement_interval));

        // 2. Notify UI via feed
        self.projection
            .send(projection::Update(self.current_offers))
            .await?;

        // 3. Inform connected takers
        self.takers
            .send_async_safe(connection::BroadcastOffers(self.current_offers))
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
                unreachable!("Handled within `collab_settlement::Actor");
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
                        RolloverVersion::V1,
                    )
                    .await
                {
                    tracing::warn!("Failed to handle rollover proposal: {:#}", e);
                }
            }
            wire::TakerToMaker::ProposeRolloverV2 {
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
                        RolloverVersion::V2,
                    )
                    .await
                {
                    tracing::warn!("Failed to handle rollover proposal V2: {:#}", e);
                }
            }
            wire::TakerToMaker::RolloverProtocol { .. }
            | wire::TakerToMaker::Protocol { .. }
            | wire::TakerToMaker::Hello(_)
            | wire::TakerToMaker::HelloV2 { .. }
            | wire::TakerToMaker::Unknown => {
                if cfg!(debug_assertions) {
                    unreachable!("Message {} is not dispatched to this actor", msg.name())
                }
            }
        }
    }
}

#[async_trait]
impl<O: Send + 'static, T: Send + 'static, W: Send + 'static> xtra::Actor for Actor<O, T, W> {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
