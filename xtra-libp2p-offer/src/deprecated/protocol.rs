use asynchronous_codec::FramedWrite;
use asynchronous_codec::JsonCodec;
use asynchronous_codec::JsonCodecError;
use futures::AsyncWriteExt;
use futures::SinkExt;
use model::olivia::BitMexPriceEventId;
use model::ContractSymbol;
use model::FundingRate;
use model::Leverage;
use model::OfferId;
use model::OpeningFee;
use model::Origin;
use model::Position;
use model::Price;
use model::Timestamp;
use model::TxFeeRate;
use model::Usd;
use serde::Deserialize;
use serde::Serialize;
use time::Duration;

pub(crate) async fn send<S>(sink: S, offers: Option<MakerOffers>) -> Result<(), JsonCodecError>
where
    S: AsyncWriteExt + Unpin,
{
    let mut framed = FramedWrite::new(sink, JsonCodec::<Option<MakerOffers>, ()>::new());
    framed.send(offers).await?;
    MESSAGES_SENT.inc();

    Ok(())
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct MakerOffers {
    long: Option<Offer>,
    short: Option<Offer>,
    tx_fee_rate: TxFeeRate,
    funding_rate_long: FundingRate,
    funding_rate_short: FundingRate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Offer {
    id: OfferId,
    #[serde(rename = "trading_pair")]
    contract_symbol: ContractSymbol,
    #[serde(rename = "position")]
    position_maker: Position,
    price: Price,
    min_quantity: Usd,
    max_quantity: Usd,
    #[serde(rename = "leverage")]
    leverage_taker: Leverage,
    leverage_choices: Vec<Leverage>,
    #[serde(rename = "creation_timestamp")]
    creation_timestamp_maker: Timestamp,
    settlement_interval: Duration,
    origin: Origin,
    oracle_event_id: BitMexPriceEventId,
    tx_fee_rate: TxFeeRate,
    funding_rate: FundingRate,
    opening_fee: OpeningFee,
}

impl From<model::Offer> for Offer {
    fn from(offer: model::Offer) -> Self {
        Self {
            id: offer.id,
            contract_symbol: offer.contract_symbol,
            position_maker: offer.position_maker,
            price: offer.price,
            min_quantity: offer.min_quantity,
            max_quantity: offer.max_quantity,
            leverage_taker: Leverage::TWO,
            leverage_choices: offer.leverage_choices,
            creation_timestamp_maker: offer.creation_timestamp_maker,
            settlement_interval: offer.settlement_interval,
            origin: Origin::Ours, // the maker always creates it
            oracle_event_id: offer.oracle_event_id,
            tx_fee_rate: offer.tx_fee_rate,
            funding_rate: offer.funding_rate,
            opening_fee: offer.opening_fee,
        }
    }
}

impl MakerOffers {
    pub(crate) fn new(offers: Vec<model::Offer>) -> Option<Self> {
        // We can safely assume that the new offers all have the same `tx_fee_rate`, because this
        // field is redundant across offers
        let tx_fee_rate = offers.first()?.tx_fee_rate;

        // This version of the protocol caters to takers that only support BTCUSD CFDs
        let mut offers = offers
            .iter()
            .filter(|offer| offer.contract_symbol == ContractSymbol::BtcUsd);

        let long = offers.find_map(|offer| {
            (offer.position_maker == Position::Long).then(|| Offer::from(offer.clone()))
        })?;
        let short = offers.find_map(|offer| {
            (offer.position_maker == Position::Short).then(|| Offer::from(offer.clone()))
        })?;

        let funding_rate_long = long.funding_rate;
        let funding_rate_short = short.funding_rate;

        Some(Self {
            long: Some(long),
            short: Some(short),
            tx_fee_rate,
            funding_rate_long,
            funding_rate_short,
        })
    }
}

static MESSAGES_SENT: conquer_once::Lazy<prometheus::IntCounter> = conquer_once::Lazy::new(|| {
    prometheus::register_int_counter!(
        "offer_messages_sent_total_v1",
        "The number of offer messages sent over the libp2p connection.",
    )
    .unwrap()
});
