use crate::cfd_actors::insert_cfd_and_update_feed;
use crate::collab_settlement_taker;
use crate::connection;
use crate::oracle;
use crate::process_manager;
use crate::projection;
use crate::setup_taker;
use crate::wallet;
use anyhow::Context as _;
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::schnorrsig;
use model::Cfd;
use model::Identity;
use model::MakerOffers;
use model::OrderId;
use model::Origin;
use model::Price;
use model::Role;
use model::Usd;
use tokio_tasks::Tasks;
use xtra::Actor as _;
use xtra_productivity::xtra_productivity;
use xtras::AddressMap;

#[derive(Clone, Copy)]
pub struct CurrentMakerOffers(pub Option<MakerOffers>);

#[derive(Clone, Copy)]
pub struct TakeOffer {
    pub order_id: OrderId,
    pub quantity: Usd,
}

#[derive(Clone, Copy)]
pub struct ProposeSettlement {
    pub order_id: OrderId,
    pub current_price: Price,
}

pub struct Actor<O, W> {
    db: sqlx::SqlitePool,
    wallet: xtra::Address<W>,
    oracle_pk: schnorrsig::PublicKey,
    projection_actor: xtra::Address<projection::Actor>,
    process_manager_actor: xtra::Address<process_manager::Actor>,
    conn_actor: xtra::Address<connection::Actor>,
    setup_actors: AddressMap<OrderId, setup_taker::Actor>,
    collab_settlement_actors: AddressMap<OrderId, collab_settlement_taker::Actor>,
    oracle_actor: xtra::Address<O>,
    n_payouts: usize,
    tasks: Tasks,
    current_maker_offers: Option<MakerOffers>,
    maker_identity: Identity,
}

impl<O, W> Actor<O, W>
where
    W: xtra::Handler<wallet::Sign> + xtra::Handler<wallet::BuildPartyParams>,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: sqlx::SqlitePool,
        wallet: xtra::Address<W>,
        oracle_pk: schnorrsig::PublicKey,
        projection_actor: xtra::Address<projection::Actor>,
        process_manager_actor: xtra::Address<process_manager::Actor>,
        conn_actor: xtra::Address<connection::Actor>,
        oracle_actor: xtra::Address<O>,
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
            current_maker_offers: None,
            maker_identity,
        }
    }
}

#[xtra_productivity]
impl<O, W> Actor<O, W> {
    async fn handle_current_offers(&mut self, msg: CurrentMakerOffers) -> Result<()> {
        let takers_perspective_of_maker_offers = msg.0.map(|mut maker_offers| {
            maker_offers.long = maker_offers.long.map(|mut long| {
                long.origin = Origin::Theirs;
                long
            });
            maker_offers.short = maker_offers.short.map(|mut short| {
                short.origin = Origin::Theirs;
                short
            });

            maker_offers
        });

        self.current_maker_offers = takers_perspective_of_maker_offers;

        tracing::trace!("new maker offers {:?}", takers_perspective_of_maker_offers);

        self.projection_actor
            .send(projection::Update(takers_perspective_of_maker_offers))
            .await?;

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

        let addr = collab_settlement_taker::Actor::new(
            order_id,
            current_price,
            self.n_payouts,
            self.conn_actor.clone(),
            self.process_manager_actor.clone(),
            self.db.clone(),
        )
        .create(None)
        .spawn(&mut self.tasks);

        disconnected.insert(addr);

        Ok(())
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

        let (order_to_take, maker_offers) = self
            .current_maker_offers
            .context("No maker offers available to take")?
            .take_order(order_id);

        let order_to_take = order_to_take.context("Order to take could not be found in current maker offers, you might have an outdated offer")?;
        self.current_maker_offers.replace(maker_offers);

        tracing::info!("Taking current order: {:?}", &order_to_take);

        // We create the cfd here without any events yet, only static data
        // Once the contract setup completes (rejected / accepted / failed) the first event will be
        // recorded
        let cfd = Cfd::from_order(order_to_take, quantity, self.maker_identity, Role::Taker);
        insert_cfd_and_update_feed(&cfd, &mut conn, &self.projection_actor).await?;

        // Cleanup own order feed, after inserting the cfd.
        // Due to the 1:1 relationship between order and cfd we can never create another cfd for the
        // same order id.
        self.projection_actor
            .send(projection::Update(self.current_maker_offers))
            .await?;

        let price_event_id = order_to_take.oracle_event_id;
        let announcement = self
            .oracle_actor
            .send(oracle::GetAnnouncement(price_event_id))
            .await?
            .with_context(|| format!("Announcement {price_event_id} not found"))?;

        let addr = setup_taker::Actor::new(
            self.db.clone(),
            self.process_manager_actor.clone(),
            (cfd.id(), cfd.quantity(), self.n_payouts),
            (self.oracle_pk, announcement),
            &self.wallet,
            &self.wallet,
            self.conn_actor.clone(),
        )
        .create(None)
        .spawn(&mut self.tasks);

        disconnected.insert(addr);

        Ok(())
    }
}

#[async_trait]
impl<O: 'static, W: 'static> xtra::Actor for Actor<O, W> {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
