use crate::collab_settlement;
use crate::collab_settlement::taker::Settle;
use crate::order;
use crate::projection;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use model::libp2p::PeerId;
use model::Cfd;
use model::Contracts;
use model::Identity;
use model::Leverage;
use model::OfferId;
use model::OrderId;
use model::{ContractSymbol, Position};
use sqlite_db;
use std::collections::HashMap;
use time::OffsetDateTime;
use xtra_productivity::xtra_productivity;
use xtras::SendAsyncSafe;

#[derive(Clone, Copy)]
pub struct PlaceOrder {
    pub offer_id: OfferId,
    pub quantity: Contracts,
    pub leverage: Leverage,
}

#[derive(Clone, Copy)]
pub struct ProposeSettlement {
    pub order_id: OrderId,
}

pub struct Actor {
    db: sqlite_db::Connection,
    projection_actor: xtra::Address<projection::Actor>,
    collab_settlement_actor: xtra::Address<collab_settlement::taker::Actor>,
    order_actor: xtra::Address<order::taker::Actor>,
    offers: Offers,
    latest_offers: HashMap<(ContractSymbol, Position), model::Offer>,
    maker_identity: Identity,
    maker_peer_id: PeerId,
}

impl Actor {
    pub fn new(
        db: sqlite_db::Connection,
        projection_actor: xtra::Address<projection::Actor>,
        collab_settlement_actor: xtra::Address<collab_settlement::taker::Actor>,
        order_actor: xtra::Address<order::taker::Actor>,
        maker_identity: Identity,
        maker_peer_id: PeerId,
    ) -> Self {
        Self {
            db,
            projection_actor,
            collab_settlement_actor,
            order_actor,
            offers: Offers::default(),
            maker_identity,
            maker_peer_id,
            latest_offers: HashMap::new(),
        }
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle_latest_offers(&mut self, msg: offer::taker::LatestOffers) {
        self.offers.insert(msg.0.clone());

        for i in msg.0.iter() {
            tracing::info!(
                "Received new offer: {:?} with price {}",
                i.position_maker.counter_position(),
                i.price
            );
            self.latest_offers.insert(
                (i.contract_symbol, i.position_maker.counter_position()),
                i.clone(),
            );
        }

        if let Err(e) = self.projection_actor.send(projection::Update(msg.0)).await {
            tracing::warn!("Failed to send current offers to projection actor: {e:#}");
        };
    }

    async fn handle_propose_settlement(&mut self, msg: ProposeSettlement) -> Result<()> {
        let ProposeSettlement { order_id } = msg;

        let cfd = self.db.load_open_cfd::<Cfd>(order_id, ()).await?;

        let offer = self
            .latest_offers
            .get(&(cfd.contract_symbol(), cfd.position().counter_position()))
            .context("Cannot propose settlement without price")?;

        if !offer.is_safe_to_take(OffsetDateTime::now_utc()) {
            bail!("The maker's offer appears to be outdated, cannot close position");
        }

        let proposal_closing_price = offer.price;
        let offer_timestamp = offer
            .creation_timestamp_maker
            .format()
            .context("Failed to format timestamp")?;

        tracing::debug!(%order_id, %proposal_closing_price, %offer_timestamp, "Proposing settlement of contract");

        // Wait for the response to check for invariants (ie. whether it is possible to settle)
        self.collab_settlement_actor
            .send(Settle {
                order_id,
                price: proposal_closing_price,
                maker_peer_id: cfd
                    .counterparty_peer_id()
                    .context("No counterparty peer id found")?,
            })
            .await??;

        Ok(())
    }

    async fn handle(&mut self, msg: PlaceOrder) -> Result<OrderId> {
        let PlaceOrder {
            offer_id,
            quantity,
            leverage,
        } = msg;

        let offer = self
            .offers
            .get(&offer_id)
            .context("Offer to take could not be found in current maker offers, you might have an outdated offer")?;

        if !offer.is_safe_to_take(OffsetDateTime::now_utc()) {
            bail!("The maker's offer appears to be outdated, refusing to place order");
        }

        let order_id = OrderId::default();
        let place_order = order::taker::PlaceOrder::new(
            order_id,
            offer,
            (quantity, leverage),
            self.maker_peer_id.inner(),
            self.maker_identity,
        );

        self.order_actor
            .send_async_safe(place_order)
            .await
            .context("Failed to place order")?;

        Ok(order_id)
    }
}

#[derive(Default)]
struct Offers(HashMap<OfferId, model::Offer>);

impl Offers {
    fn insert(&mut self, offers: Vec<model::Offer>) {
        for offer in offers.into_iter() {
            self.0.insert(offer.id, offer);
        }
    }

    fn get(&mut self, id: &OfferId) -> Option<model::Offer> {
        self.remove_old_offers();

        self.0.get(id).cloned()
    }

    fn remove_old_offers(&mut self) {
        self.0
            .retain(|_, offer| offer.is_safe_to_take(OffsetDateTime::now_utc()));
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
