use crate::metrics::time_to_first_position;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use daemon::order;
use daemon::projection;
use model::ContractSymbol;
use model::Contracts;
use model::FundingRate;
use model::Identity;
use model::Leverage;
use model::LotSize;
use model::OpeningFee;
use model::OrderId;
use model::Position;
use model::Price;
use model::Timestamp;
use model::TxFeeRate;
use nonempty::NonEmpty;
use std::collections::HashMap;
use time::Duration;
use time::OffsetDateTime;
use xtra::prelude::MessageChannel;
use xtra_productivity::xtra_productivity;
use xtras::SendAsyncSafe;

const ROLLOVER_PARAMS_TTL: Duration = Duration::minutes(5);

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
pub struct GetRolloverParams(ContractSymbol);

#[derive(Clone, Debug)]
pub struct OfferParams {
    pub price_long: Option<Price>,
    pub price_short: Option<Price>,
    pub min_quantity: Contracts,
    pub max_quantity: Contracts,
    pub tx_fee_rate: TxFeeRate,
    pub funding_rate_long: FundingRate,
    pub funding_rate_short: FundingRate,
    pub opening_fee: OpeningFee,
    pub leverage_choices: Vec<Leverage>,
    pub contract_symbol: ContractSymbol,
    pub lot_size: LotSize,
}

impl OfferParams {
    fn into_offers(self, settlement_interval: Duration) -> Vec<model::Offer> {
        let Self {
            price_long,
            price_short,
            min_quantity,
            max_quantity,
            tx_fee_rate,
            funding_rate_long,
            funding_rate_short,
            opening_fee,
            leverage_choices,
            contract_symbol,
            lot_size,
        } = self;

        let mut offers = Vec::new();

        if let Some(price_long) = price_long {
            let long = model::Offer::new(
                Position::Long,
                price_long,
                min_quantity,
                max_quantity,
                settlement_interval,
                tx_fee_rate,
                funding_rate_long,
                opening_fee,
                leverage_choices.clone(),
                contract_symbol,
                lot_size,
            );

            offers.push(long);
        }

        if let Some(price_short) = price_short {
            let short = model::Offer::new(
                Position::Short,
                price_short,
                min_quantity,
                max_quantity,
                settlement_interval,
                tx_fee_rate,
                funding_rate_short,
                opening_fee,
                leverage_choices,
                contract_symbol,
                lot_size,
            );

            offers.push(short);
        }

        offers
    }
}

/// Proposed rollover
#[derive(Debug, Clone, PartialEq)]
struct RolloverProposal {
    pub order_id: OrderId,
    pub timestamp: Timestamp,
}

#[derive(Default, Clone)]
struct RolloverParams {
    funding_rates: HashMap<ContractSymbol, (FundingRates, OffsetDateTime)>,
    tx_fee_rate: TxFeeRate,
}

#[derive(Clone, Copy)]
pub struct FundingRates {
    long: FundingRate,
    short: FundingRate,
}

pub struct Actor {
    settlement_interval: Duration,
    projection: xtra::Address<projection::Actor>,
    rollover_params: RolloverParams,
    time_to_first_position: xtra::Address<time_to_first_position::Actor>,
    libp2p_collab_settlement: xtra::Address<daemon::collab_settlement::maker::Actor>,
    libp2p_offer: xtra::Address<xtra_libp2p_offer::maker::Actor>,
    libp2p_offer_deprecated: xtra::Address<xtra_libp2p_offer::deprecated::maker::Actor>,
    order: xtra::Address<order::maker::Actor>,
}

impl Actor {
    pub fn new(
        settlement_interval: Duration,
        projection: xtra::Address<projection::Actor>,
        time_to_first_position: xtra::Address<time_to_first_position::Actor>,
        libp2p_collab_settlement: xtra::Address<daemon::collab_settlement::maker::Actor>,
        (libp2p_offer, libp2p_offer_deprecated): (
            xtra::Address<xtra_libp2p_offer::maker::Actor>,
            xtra::Address<xtra_libp2p_offer::deprecated::maker::Actor>,
        ),
        order: xtra::Address<order::maker::Actor>,
    ) -> Self {
        Self {
            settlement_interval,
            projection,
            rollover_params: RolloverParams::default(),
            time_to_first_position,
            libp2p_collab_settlement,
            libp2p_offer,
            libp2p_offer_deprecated,
            order,
        }
    }

    fn udpate_rollover_params(
        &mut self,
        contract_symbol: ContractSymbol,
        long: FundingRate,
        short: FundingRate,
        tx_fee_rate: TxFeeRate,
    ) {
        let funding_rates = FundingRates { long, short };
        let expiry = OffsetDateTime::now_utc() + ROLLOVER_PARAMS_TTL;

        self.rollover_params
            .funding_rates
            .insert(contract_symbol, (funding_rates, expiry));
        self.rollover_params.tx_fee_rate = tx_fee_rate;
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

    async fn handle(
        &mut self,
        GetRolloverParams(contract_symbol): GetRolloverParams,
    ) -> Result<(FundingRates, TxFeeRate)> {
        let (funding_rates, expiry) = *self
            .rollover_params
            .funding_rates
            .get(&contract_symbol)
            .with_context(|| format!("Missing {contract_symbol} funding rates"))?;

        if expiry < OffsetDateTime::now_utc() {
            bail!("Outdated funding rates");
        }

        let tx_fee_rate = self.rollover_params.tx_fee_rate;

        Ok((funding_rates, tx_fee_rate))
    }
}

#[xtra_productivity]
impl Actor {
    async fn handle_offer_params(&mut self, offer_params: OfferParams) -> Result<()> {
        // 1. Update internal state for rollovers
        self.udpate_rollover_params(
            offer_params.contract_symbol,
            offer_params.funding_rate_long,
            offer_params.funding_rate_short,
            offer_params.tx_fee_rate,
        );

        let offers = offer_params.into_offers(self.settlement_interval);

        // 2. Notify UI via feed
        self.projection
            .send(projection::Update(offers.clone()))
            .await?;

        // 3. Broadcast to all peers via offer actor
        if let Err(e) = self
            .libp2p_offer
            .send_async_safe(xtra_libp2p_offer::maker::NewOffers::new(offers.clone()))
            .await
        {
            tracing::warn!("{e:#}");
        }

        // 4. Broadcast to all peers via deprecated offer actor
        {
            // Takers on the deprecated version only care (and know how to handle) BTCUSD offers
            let btcusd_offers = offers
                .into_iter()
                .filter(|offer| offer.contract_symbol == ContractSymbol::BtcUsd)
                .collect::<Vec<_>>();

            if let Some(btcusd_offers) = NonEmpty::from_vec(btcusd_offers) {
                if let Err(e) = self
                    .libp2p_offer_deprecated
                    .send_async_safe(xtra_libp2p_offer::deprecated::maker::NewOffers::new(
                        btcusd_offers,
                    ))
                    .await
                {
                    tracing::warn!("{e:#}");
                }
            };
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
pub struct RatesChannel(MessageChannel<GetRolloverParams, Result<(FundingRates, TxFeeRate)>>);

impl RatesChannel {
    pub fn new(
        channel: MessageChannel<GetRolloverParams, Result<(FundingRates, TxFeeRate)>>,
    ) -> Self {
        Self(channel)
    }
}

#[async_trait]
impl rollover::current::protocol::GetRates for RatesChannel {
    async fn get_rates(
        &self,
        contract_symbol: ContractSymbol,
    ) -> Result<rollover::current::protocol::Rates> {
        let (FundingRates { long, short }, tx_fee_rate) = self
            .0
            .send(GetRolloverParams(contract_symbol))
            .await
            .context("CFD actor disconnected")??;

        Ok(rollover::current::protocol::Rates::new(
            long,
            short,
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
        let (FundingRates { long, short }, tx_fee_rate) = self
            .0
            .send(GetRolloverParams(contract_symbol))
            .await
            .context("CFD actor disconnected")??;

        Ok(rollover::protocol::Rates::new(long, short, tx_fee_rate))
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}
