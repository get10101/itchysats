use crate::metrics::time_to_first_position;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use daemon::order;
use daemon::projection;
use model::olivia;
use model::olivia::BitMexPriceEventId;
use model::ContractSymbol;
use model::FundingRate;
use model::Identity;
use model::Leverage;
use model::MakerOffers;
use model::Offer;
use model::OpeningFee;
use model::OrderId;
use model::Origin;
use model::Position;
use model::Price;
use model::Timestamp;
use model::TxFeeRate;
use model::Usd;
use std::collections::HashMap;
use time::Duration;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;
use xtras::SendAsyncSafe;

#[derive(Clone)]
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
pub struct TakerConnected {
    pub id: Identity,
}

#[derive(Clone, Copy)]
pub struct TakerDisconnected {
    pub id: Identity,
}

#[derive(Clone, Copy)]
pub struct GetOffers;

#[derive(Clone, Debug)]
pub struct OfferParams {
    pub price_long: Option<Price>,
    pub price_short: Option<Price>,
    pub min_quantity: Usd,
    pub max_quantity: Usd,
    pub tx_fee_rate: TxFeeRate,
    pub funding_rate_long: FundingRate,
    pub funding_rate_short: FundingRate,
    pub opening_fee: OpeningFee,
    pub leverage_choices: Vec<Leverage>,
    pub contract_symbol: ContractSymbol,
}

impl OfferParams {
    fn pick_oracle_event_id(settlement_interval: Duration) -> BitMexPriceEventId {
        olivia::next_announcement_after(time::OffsetDateTime::now_utc() + settlement_interval)
    }

    pub fn create_long_order(&self, settlement_interval: Duration) -> Option<Offer> {
        self.price_long.map(|price_long| {
            Offer::new(
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
                self.leverage_choices.clone(),
                self.contract_symbol,
            )
        })
    }

    pub fn create_short_order(&self, settlement_interval: Duration) -> Option<Offer> {
        self.price_short.map(|price_short| {
            Offer::new(
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
                self.leverage_choices.clone(),
                self.contract_symbol,
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

/// Proposed rollover
#[derive(Debug, Clone, PartialEq)]
struct RolloverProposal {
    pub order_id: OrderId,
    pub timestamp: Timestamp,
}

pub struct Actor {
    settlement_interval: Duration,
    projection: xtra::Address<projection::Actor>,
    current_offers: HashMap<ContractSymbol, MakerOffers>,
    time_to_first_position: xtra::Address<time_to_first_position::Actor>,
    libp2p_collab_settlement: xtra::Address<daemon::collab_settlement::maker::Actor>,
    libp2p_offer: xtra::Address<xtra_libp2p_offer::maker::Actor>,
    order: xtra::Address<order::maker::Actor>,
}

impl Actor {
    pub fn new(
        settlement_interval: Duration,
        projection: xtra::Address<projection::Actor>,
        time_to_first_position: xtra::Address<time_to_first_position::Actor>,
        libp2p_collab_settlement: xtra::Address<daemon::collab_settlement::maker::Actor>,
        libp2p_offer: xtra::Address<xtra_libp2p_offer::maker::Actor>,
        order: xtra::Address<order::maker::Actor>,
    ) -> Self {
        Self {
            settlement_interval,
            projection,
            current_offers: HashMap::new(),
            time_to_first_position,
            libp2p_collab_settlement,
            libp2p_offer,
            order,
        }
    }
}

impl Actor {
    async fn handle_taker_connected(&mut self, taker_id: Identity) -> Result<()> {
        self.time_to_first_position
            .send_async_safe(time_to_first_position::Connected::new(taker_id))
            .await?;
        Ok(())
    }

    async fn handle_taker_disconnected(&mut self, _taker_id: Identity) -> Result<()> {
        Ok(())
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle_accept_order(&mut self, msg: AcceptOrder) -> Result<()> {
        let AcceptOrder { order_id } = msg;

        self.order
            .send(order::maker::Decision::Accept(order_id))
            .await??;

        Ok(())
    }

    async fn handle_reject_order(&mut self, msg: RejectOrder) -> Result<()> {
        let RejectOrder { order_id } = msg;

        self.order
            .send(order::maker::Decision::Reject(order_id))
            .await??;

        Ok(())
    }

    async fn handle_accept_settlement(&mut self, msg: AcceptSettlement) -> Result<()> {
        let AcceptSettlement { order_id } = msg;

        self.libp2p_collab_settlement
            .send(daemon::collab_settlement::maker::Accept { order_id })
            .await??;

        Ok(())
    }

    async fn handle_reject_settlement(&mut self, msg: RejectSettlement) -> Result<()> {
        let RejectSettlement { order_id } = msg;

        self.libp2p_collab_settlement
            .send(daemon::collab_settlement::maker::Reject { order_id })
            .await??;

        Ok(())
    }

    async fn handle(&mut self, _: GetOffers) -> HashMap<ContractSymbol, MakerOffers> {
        self.current_offers.clone()
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle_offer_params(&mut self, msg: OfferParams) -> Result<()> {
        // 1. Update actor state to current order
        let maker_offers = create_maker_offers(msg.clone(), self.settlement_interval);
        self.current_offers
            .insert(msg.contract_symbol, maker_offers.clone());

        // 2. Notify UI via feed
        self.projection
            .send(projection::Update(Some(maker_offers.clone())))
            .await?;

        if let Err(e) = self
            .libp2p_offer
            .send_async_safe(xtra_libp2p_offer::maker::NewOffers::new(Some(
                maker_offers.into(),
            )))
            .await
        {
            tracing::warn!("{e:#}");
        }

        Ok(())
    }

    async fn handle(&mut self, msg: TakerConnected) -> Result<()> {
        self.handle_taker_connected(msg.id).await
    }

    async fn handle(&mut self, msg: TakerDisconnected) -> Result<()> {
        self.handle_taker_disconnected(msg.id).await
    }
}

/// Source of offer rates used for rolling over CFDs.
#[derive(Clone)]
pub struct RatesChannel(MessageChannel<GetOffers, HashMap<ContractSymbol, MakerOffers>>);

impl RatesChannel {
    pub fn new(channel: MessageChannel<GetOffers, HashMap<ContractSymbol, MakerOffers>>) -> Self {
        Self(channel)
    }
}

#[async_trait]
impl rollover::deprecated::protocol::GetRates for RatesChannel {
    async fn get_rates(&self) -> Result<rollover::deprecated::protocol::Rates> {
        let MakerOffers {
            funding_rate_long,
            funding_rate_short,
            tx_fee_rate,
            ..
        } = self
            .0
            .send(GetOffers)
            .await
            .context("CFD actor disconnected")?
            .get(&ContractSymbol::BtcUsd)
            .context("No up-to-date rates")?
            .clone();

        Ok(rollover::deprecated::protocol::Rates::new(
            funding_rate_long,
            funding_rate_short,
            tx_fee_rate,
        ))
    }
}

#[async_trait]
impl rollover::protocol::GetRates for RatesChannel {
    async fn get_rates(
        &self,
        contract_symbol: ContractSymbol,
    ) -> Result<rollover::protocol::Rates> {
        let MakerOffers {
            funding_rate_long,
            funding_rate_short,
            tx_fee_rate,
            ..
        } = self
            .0
            .send(GetOffers)
            .await
            .context("CFD actor disconnected")?
            .get(&contract_symbol)
            .context("No up-to-date rates")?
            .clone();

        Ok(rollover::protocol::Rates::new(
            funding_rate_long,
            funding_rate_short,
            tx_fee_rate,
        ))
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
