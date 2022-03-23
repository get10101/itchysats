use crate::collab_settlement_taker;
use crate::connection;
use crate::db;
use crate::oracle;
use crate::process_manager;
use crate::projection;
use crate::setup_taker;
use crate::wallet;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::schnorrsig;
use model::closing_price;
use model::Cfd;
use model::Identity;
use model::MakerOffers;
use model::OrderId;
use model::Origin;
use model::Price;
use model::Role;
use model::Usd;
use time::OffsetDateTime;
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

#[derive(Clone)]
pub struct ProposeSettlement {
    pub order_id: OrderId,
    pub bid: Price,
    pub ask: Price,
    pub quote_timestamp: String,
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
            bid,
            ask,
            quote_timestamp,
        } = msg;

        let mut conn = self.db.acquire().await?;

        #[allow(deprecated)]
        // We need the position and role to decide the price here. Consider re-modelling access to
        // quote / where to taker the decision to avoid this call.
        let cfd = crate::command::load_cfd(order_id, &mut conn).await?;

        let proposal_closing_price = closing_price(bid, ask, Role::Taker, cfd.position());

        let disconnected = self
            .collab_settlement_actors
            .get_disconnected(order_id)
            .with_context(|| format!("Settlement for order {order_id} is already in progress"))?;

        tracing::debug!(%order_id, %proposal_closing_price, %bid, %ask, %quote_timestamp, "Proposing settlement of contract");

        let addr = collab_settlement_taker::Actor::new(
            order_id,
            proposal_closing_price,
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

        // The offer we are instructed to take is removed from the
        // set of available offers immediately so that we don't attempt
        // to take it more than once
        {
            self.current_maker_offers.replace(maker_offers);
            self.projection_actor
                .send(projection::Update(self.current_maker_offers))
                .await?;
        }

        if !order_to_take.is_safe_to_take(OffsetDateTime::now_utc()) {
            bail!("The maker's offer appears to be outdated, refusing to take offer",);
        }

        tracing::info!("Taking current order: {:?}", &order_to_take);

        // We create the cfd here without any events yet, only static data
        // Once the contract setup completes (rejected / accepted / failed) the first event will be
        // recorded
        let cfd = Cfd::from_order(order_to_take, quantity, self.maker_identity, Role::Taker);

        db::insert_cfd(&cfd, &mut conn).await?;
        self.projection_actor
            .send(projection::CfdChanged(cfd.id()))
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
