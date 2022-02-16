use crate::cfd_actors::insert_cfd_and_update_feed;
use crate::collab_settlement_maker;
use crate::command;
use crate::maker_inc_connections;
use crate::oracle;
use crate::process_manager;
use crate::projection;
use crate::projection::Update;
use crate::rollover_maker;
use crate::setup_maker;
use crate::wallet;
use crate::wire;
use crate::wire::TakerToMaker;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::schnorrsig;
use model::Cfd;
use model::CollaborativeSettlementCompleted;
use model::FundingRate;
use model::Identity;
use model::OpeningFee;
use model::Order;
use model::OrderId;
use model::Origin;
use model::Position;
use model::Price;
use model::Role;
use model::SettlementProposal;
use model::SetupCompleted;
use model::Timestamp;
use model::TxFeeRate;
use model::Usd;
use std::collections::HashSet;
use time::Duration;
use tokio_tasks::Tasks;
use xtra::prelude::*;
use xtra::Actor as _;
use xtra_productivity::xtra_productivity;
use xtras::address_map::Stopping;
use xtras::AddressMap;
use xtras::SendAsyncSafe;

pub struct AcceptOrder {
    pub order_id: OrderId,
}
pub struct RejectOrder {
    pub order_id: OrderId,
}
pub struct AcceptSettlement {
    pub order_id: OrderId,
}
pub struct RejectSettlement {
    pub order_id: OrderId,
}
pub struct AcceptRollover {
    pub order_id: OrderId,
}
pub struct RejectRollover {
    pub order_id: OrderId,
}
pub struct NewOrder {
    pub price: Price,
    pub min_quantity: Usd,
    pub max_quantity: Usd,
    pub tx_fee_rate: TxFeeRate,
    pub funding_rate: FundingRate,
    pub opening_fee: OpeningFee,
    pub position: Position,
}

pub struct TakerConnected {
    pub id: Identity,
}

pub struct TakerDisconnected {
    pub id: Identity,
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
    db: sqlx::SqlitePool,
    wallet: Address<W>,
    settlement_interval: Duration,
    oracle_pk: schnorrsig::PublicKey,
    projection: Address<projection::Actor>,
    process_manager: Address<process_manager::Actor>,
    executor: command::Executor,
    rollover_actors: AddressMap<OrderId, rollover_maker::Actor>,
    takers: Address<T>,
    current_order: Option<Order>,
    setup_actors: AddressMap<OrderId, setup_maker::Actor>,
    settlement_actors: AddressMap<OrderId, collab_settlement_maker::Actor>,
    oracle: Address<O>,
    connected_takers: HashSet<Identity>,
    n_payouts: usize,
    tasks: Tasks,
}

impl<O, T, W> Actor<O, T, W> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: sqlx::SqlitePool,
        wallet: Address<W>,
        settlement_interval: Duration,
        oracle_pk: schnorrsig::PublicKey,
        projection: Address<projection::Actor>,
        process_manager: Address<process_manager::Actor>,
        takers: Address<T>,
        oracle: Address<O>,
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
            current_order: None,
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
            .send(Update(
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
                msg: wire::MakerToTaker::CurrentOrder(self.current_order.clone()),
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
        + xtra::Handler<Stopping<rollover_maker::Actor>>
        + xtra::Handler<maker_inc_connections::RegisterRollover>,
    W: 'static,
    Self: xtra::Handler<Stopping<rollover_maker::Actor>>,
{
    async fn handle_propose_rollover(
        &mut self,
        RolloverProposal { order_id, .. }: RolloverProposal,
        taker_id: Identity,
        ctx: &mut xtra::Context<Self>,
    ) -> Result<()> {
        tracing::info!(%order_id, %taker_id,  "Received rollover proposal from taker");
        let this = ctx.address().expect("acquired own address");

        let (rollover_actor_addr, rollover_actor_future) = rollover_maker::Actor::new(
            order_id,
            self.n_payouts,
            &self.takers,
            taker_id,
            self.oracle_pk,
            &self.oracle,
            (&self.takers, &this),
            self.process_manager.clone(),
            &self.takers,
            self.db.clone(),
        )
        .create(None)
        .run();

        self.tasks.add(rollover_actor_future);

        self.rollover_actors.insert(order_id, rollover_actor_addr);

        Ok(())
    }
}

impl<O, T, W> Actor<O, T, W>
where
    O: xtra::Handler<oracle::GetAnnouncement> + xtra::Handler<oracle::MonitorAttestation>,
    T: xtra::Handler<maker_inc_connections::ConfirmOrder>
        + xtra::Handler<maker_inc_connections::TakerMessage>
        + xtra::Handler<maker_inc_connections::BroadcastOrder>
        + xtra::Handler<Stopping<setup_maker::Actor>>,
    W: xtra::Handler<wallet::Sign> + xtra::Handler<wallet::BuildPartyParams>,
{
    async fn handle_take_order(
        &mut self,
        taker_id: Identity,
        order_id: OrderId,
        quantity: Usd,
        ctx: &mut xtra::Context<Self>,
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
        let current_order = match &self.current_order {
            Some(current_order) if current_order.id == order_id => current_order.clone(),
            _ => {
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
            }
        };

        let cfd = Cfd::from_order(
            current_order.clone(),
            current_order.position,
            quantity,
            taker_id,
            Role::Maker,
        );

        // 2. Remove current order
        // The order is removed before we update the state, because the maker might react on the
        // state change. Once we know that we go for either an accept/reject scenario we
        // have to remove the current order.
        self.current_order = None;

        self.takers
            .send_async_safe(maker_inc_connections::BroadcastOrder(None))
            .await?;

        self.projection
            .send(projection::Update(Option::<Order>::None))
            .await?;
        insert_cfd_and_update_feed(&cfd, &mut conn, &self.projection).await?;

        // 4. Try to get the oracle announcement, if that fails we should exit prior to changing any
        // state
        let announcement = self
            .oracle
            .send(oracle::GetAnnouncement(current_order.oracle_event_id))
            .await??;

        // 5. Start up contract setup actor
        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");

        let (addr, fut) = setup_maker::Actor::new(
            self.db.clone(),
            self.process_manager.clone(),
            (current_order, cfd.quantity(), self.n_payouts),
            (self.oracle_pk, announcement),
            &self.wallet,
            &self.wallet,
            (&self.takers, &self.takers, taker_id),
            (&self.takers, &this),
        )
        .create(None)
        .run();

        disconnected.insert(addr);

        self.tasks.add(fut);

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
            .await
        {
            self.executor
                .execute(order_id, |cfd| {
                    cfd.setup_contract(SetupCompleted::Failed {
                        order_id,
                        error: anyhow!(error),
                    })
                })
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
            .await
        {
            self.executor
                .execute(order_id, |cfd| {
                    cfd.setup_contract(SetupCompleted::Failed {
                        order_id,
                        error: anyhow!(error),
                    })
                })
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
            .await
        {
            self.executor
                .execute(order_id, |cfd| {
                    cfd.settle_collaboratively(CollaborativeSettlementCompleted::Failed {
                        order_id,
                        error: anyhow!(error),
                    })
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
            .await
        {
            self.executor
                .execute(order_id, |cfd| {
                    cfd.settle_collaboratively(CollaborativeSettlementCompleted::Failed {
                        order_id,
                        error: anyhow!(error),
                    })
                })
                .await?;

            bail!("Reject failed: No settlement in progress for order {order_id}")
        }

        Ok(())
    }

    async fn handle_accept_rollover(&mut self, msg: AcceptRollover) -> Result<()> {
        let order = self
            .current_order
            .as_ref()
            .context("Cannot accept rollover without current offer, as we need up-to-date fees")?;

        let order_id = msg.order_id;

        if let Err(error) = self
            .rollover_actors
            .send(
                &order_id,
                rollover_maker::AcceptRollover {
                    tx_fee_rate: order.tx_fee_rate,
                    funding_rate: order.funding_rate,
                },
            )
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

#[xtra_productivity(message_impl = false)]
impl<O, T, W> Actor<O, T, W> {
    async fn handle_setup_actor_stopping(&mut self, message: Stopping<setup_maker::Actor>) {
        self.setup_actors.gc(message);
    }

    async fn handle_settlement_actor_stopping(
        &mut self,
        message: Stopping<collab_settlement_maker::Actor>,
    ) {
        self.settlement_actors.gc(message);
    }

    async fn handle_rollover_actor_stopping(&mut self, msg: Stopping<rollover_maker::Actor>) {
        self.rollover_actors.gc(msg);
    }
}

impl<O, T, W> Actor<O, T, W>
where
    O: xtra::Handler<oracle::MonitorAttestation>,
    T: xtra::Handler<maker_inc_connections::settlement::Response>
        + xtra::Handler<Stopping<collab_settlement_maker::Actor>>,
    W: 'static + Send,
{
    async fn handle_propose_settlement(
        &mut self,
        taker_id: Identity,
        proposal: SettlementProposal,
        ctx: &mut xtra::Context<Self>,
    ) -> Result<()> {
        let order_id = proposal.order_id;

        let disconnected = self
            .settlement_actors
            .get_disconnected(order_id)
            .with_context(|| format!("Settlement for order {order_id} is already in progress",))?;

        let this = ctx.address().expect("self to be alive");
        let (addr, task) = collab_settlement_maker::Actor::new(
            proposal,
            taker_id,
            &self.takers,
            self.process_manager.clone(),
            (&self.takers, &this),
            self.db.clone(),
            self.n_payouts,
        )
        .create(None)
        .run();

        self.tasks.add(task);
        disconnected.insert(addr);

        Ok(())
    }
}

#[xtra_productivity]
impl<O, T, W> Actor<O, T, W>
where
    T: xtra::Handler<maker_inc_connections::BroadcastOrder>,
{
    async fn handle_new_order(&mut self, msg: NewOrder) -> Result<()> {
        let NewOrder {
            price,
            min_quantity,
            max_quantity,
            tx_fee_rate,
            funding_rate,
            opening_fee,
            position,
        } = msg;

        let oracle_event_id = oracle::next_announcement_after(
            time::OffsetDateTime::now_utc() + self.settlement_interval,
        )?;

        let order = Order::new(
            position,
            price,
            min_quantity,
            max_quantity,
            Origin::Ours,
            oracle_event_id,
            self.settlement_interval,
            tx_fee_rate,
            funding_rate,
            opening_fee,
        )?;

        // 1. Update actor state to current order
        self.current_order.replace(order.clone());

        // 2. Notify UI via feed
        self.projection
            .send(projection::Update(Some(order.clone())))
            .await?;

        // 3. Inform connected takers
        self.takers
            .send_async_safe(maker_inc_connections::BroadcastOrder(Some(order)))
            .await?;

        Ok(())
    }
}

#[async_trait]
impl<O: 'static, T: 'static, W: 'static> Handler<TakerConnected> for Actor<O, T, W>
where
    T: xtra::Handler<maker_inc_connections::TakerMessage>,
{
    async fn handle(&mut self, msg: TakerConnected, _ctx: &mut xtra::Context<Self>) -> Result<()> {
        self.handle_taker_connected(msg.id).await
    }
}

#[async_trait]
impl<O: 'static, T: 'static, W: 'static> Handler<TakerDisconnected> for Actor<O, T, W>
where
    T: xtra::Handler<maker_inc_connections::TakerMessage>,
{
    async fn handle(
        &mut self,
        msg: TakerDisconnected,
        _ctx: &mut xtra::Context<Self>,
    ) -> Result<()> {
        self.handle_taker_disconnected(msg.id).await
    }
}

#[async_trait]
impl<O: 'static, T: 'static, W: 'static> Handler<FromTaker> for Actor<O, T, W>
where
    O: xtra::Handler<oracle::GetAnnouncement> + xtra::Handler<oracle::MonitorAttestation>,
    T: xtra::Handler<maker_inc_connections::ConfirmOrder>
        + xtra::Handler<maker_inc_connections::TakerMessage>
        + xtra::Handler<maker_inc_connections::BroadcastOrder>
        + xtra::Handler<Stopping<setup_maker::Actor>>
        + xtra::Handler<Stopping<rollover_maker::Actor>>
        + xtra::Handler<maker_inc_connections::settlement::Response>
        + xtra::Handler<Stopping<collab_settlement_maker::Actor>>
        + xtra::Handler<maker_inc_connections::RegisterRollover>,
    W: xtra::Handler<wallet::Sign> + xtra::Handler<wallet::BuildPartyParams>,
{
    async fn handle(
        &mut self,
        FromTaker { taker_id, msg }: FromTaker,
        ctx: &mut xtra::Context<Self>,
    ) {
        match msg {
            wire::TakerToMaker::TakeOrder { order_id, quantity } => {
                if let Err(e) = self
                    .handle_take_order(taker_id, order_id, quantity, ctx)
                    .await
                {
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
                        ctx,
                    )
                    .await
                {
                    tracing::warn!("Failed ot handle settlement proposal: {:#}", e);
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
                        ctx,
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
            TakerToMaker::Hello(_) => {
                unreachable!("The Hello message is not sent to the cfd actor")
            }
        }
    }
}

impl Message for TakerConnected {
    type Result = Result<()>;
}

impl Message for TakerDisconnected {
    type Result = Result<()>;
}

impl Message for FromTaker {
    type Result = ();
}

#[async_trait]
impl<O: 'static, T: 'static, W: 'static> xtra::Actor for Actor<O, T, W> {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
