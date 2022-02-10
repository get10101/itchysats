use crate::cfd_actors;
use crate::cfd_actors::insert_cfd_and_update_feed;
use crate::collab_settlement_taker;
use crate::connection;
use crate::model::cfd::Cfd;
use crate::model::cfd::Order;
use crate::model::cfd::OrderId;
use crate::model::cfd::Origin;
use crate::model::cfd::Role;
use crate::model::Identity;
use crate::model::Position;
use crate::model::Price;
use crate::model::Usd;
use crate::oracle;
use crate::process_manager;
use crate::projection;
use crate::setup_taker;
use crate::wallet;
use anyhow::Context as _;
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::schnorrsig;
use tokio_tasks::Tasks;
use xtra::prelude::*;
use xtra::Actor as _;
use xtra_productivity::xtra_productivity;
use xtras::AddressMap;

pub struct CurrentOrder(pub Option<Order>);

pub struct TakeOffer {
    pub order_id: OrderId,
    pub quantity: Usd,
}

pub struct ProposeSettlement {
    pub order_id: OrderId,
    pub current_price: Price,
}

pub struct Actor<O, W> {
    db: sqlx::SqlitePool,
    wallet: Address<W>,
    oracle_pk: schnorrsig::PublicKey,
    projection_actor: Address<projection::Actor>,
    process_manager_actor: Address<process_manager::Actor>,
    conn_actor: Address<connection::Actor>,
    setup_actors: AddressMap<OrderId, setup_taker::Actor>,
    collab_settlement_actors: AddressMap<OrderId, collab_settlement_taker::Actor>,
    oracle_actor: Address<O>,
    n_payouts: usize,
    tasks: Tasks,
    current_order: Option<Order>,
    maker_identity: Identity,
}

impl<O, W> Actor<O, W>
where
    W: xtra::Handler<wallet::Sign> + xtra::Handler<wallet::BuildPartyParams>,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: sqlx::SqlitePool,
        wallet: Address<W>,
        oracle_pk: schnorrsig::PublicKey,
        projection_actor: Address<projection::Actor>,
        process_manager_actor: Address<process_manager::Actor>,
        conn_actor: Address<connection::Actor>,
        oracle_actor: Address<O>,
        n_payouts: usize,
        maker_identity: Identity,
    ) -> Self {
        Self {
            db,
            wallet,
            oracle_pk,
            projection_actor,
            process_manager_actor,
            conn_actor,
            oracle_actor,
            n_payouts,
            setup_actors: AddressMap::default(),
            collab_settlement_actors: AddressMap::default(),
            tasks: Tasks::default(),
            current_order: None,
            maker_identity,
        }
    }
}

#[xtra_productivity]
impl<O, W> Actor<O, W> {
    async fn handle_current_order(&mut self, msg: CurrentOrder) -> Result<()> {
        let order = msg.0;

        tracing::trace!("new order {:?}", order);
        match order {
            Some(mut order) => {
                order.origin = Origin::Theirs;

                self.current_order = Some(order.clone());

                self.projection_actor
                    .send(projection::Update(Some(order)))
                    .await?;
            }
            None => {
                self.projection_actor
                    .send(projection::Update(Option::<Order>::None))
                    .await?;
            }
        }
        Ok(())
    }

    async fn handle_propose_settlement(&mut self, msg: ProposeSettlement) -> Result<()> {
        let ProposeSettlement {
            order_id,
            current_price,
        } = msg;

        let disconnected = self
            .collab_settlement_actors
            .get_disconnected(order_id)
            .with_context(|| format!("Settlement for order {order_id} is already in progress"))?;

        let (addr, fut) = collab_settlement_taker::Actor::new(
            order_id,
            current_price,
            self.n_payouts,
            self.conn_actor.clone(),
            self.process_manager_actor.clone(),
            self.db.clone(),
        )
        .create(None)
        .run();

        self.tasks.add(fut);
        disconnected.insert(addr);

        Ok(())
    }
}

#[xtra_productivity(message_impl = false)]
impl<O, W> Actor<O, W> {
    async fn handle_attestation(&mut self, msg: oracle::Attestation) {
        if let Err(e) = cfd_actors::handle_oracle_attestation(
            msg.as_inner(),
            &self.db,
            &self.process_manager_actor,
        )
        .await
        {
            tracing::warn!("Failed to handle oracle attestation: {:#}", e)
        }
    }
}

#[xtra_productivity]
impl<O, W> Actor<O, W>
where
    O: xtra::Handler<oracle::GetAnnouncement> + xtra::Handler<oracle::MonitorAttestation>,
    W: xtra::Handler<wallet::BuildPartyParams> + xtra::Handler<wallet::Sign>,
{
    async fn handle_take_offer(&mut self, msg: TakeOffer) -> Result<()> {
        let TakeOffer { order_id, quantity } = msg;

        let disconnected = self
            .setup_actors
            .get_disconnected(order_id)
            .with_context(|| {
                format!("Contract setup for order {order_id} is already in progress")
            })?;

        let mut conn = self.db.acquire().await?;

        let current_order = self
            .current_order
            .clone()
            .context("No current order from maker")?;

        tracing::info!("Taking current order: {:?}", &current_order);

        // We create the cfd here without any events yet, only static data
        // Once the contract setup completes (rejected / accepted / failed) the first event will be
        // recorded
        let cfd = Cfd::from_order(
            current_order.clone(),
            Position::Long,
            quantity,
            self.maker_identity,
            Role::Taker,
        );

        insert_cfd_and_update_feed(&cfd, &mut conn, &self.projection_actor).await?;

        // Cleanup own order feed, after inserting the cfd.
        // Due to the 1:1 relationship between order and cfd we can never create another cfd for the
        // same order id.
        self.projection_actor
            .send(projection::Update(Option::<Order>::None))
            .await?;

        let price_event_id = current_order.oracle_event_id;
        let announcement = self
            .oracle_actor
            .send(oracle::GetAnnouncement(price_event_id))
            .await?
            .with_context(|| format!("Announcement {price_event_id} not found"))?;

        let (addr, fut) = setup_taker::Actor::new(
            self.db.clone(),
            self.process_manager_actor.clone(),
            (cfd.id(), cfd.quantity(), self.n_payouts),
            (self.oracle_pk, announcement),
            &self.wallet,
            &self.wallet,
            self.conn_actor.clone(),
        )
        .create(None)
        .run();

        disconnected.insert(addr);

        self.tasks.add(fut);

        Ok(())
    }
}

#[async_trait]
impl<O: 'static, W: 'static> xtra::Actor for Actor<O, W> {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
