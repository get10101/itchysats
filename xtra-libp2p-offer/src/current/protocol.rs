use asynchronous_codec::FramedRead;
use asynchronous_codec::FramedWrite;
use asynchronous_codec::JsonCodec;
use asynchronous_codec::JsonCodecError;
use futures::AsyncReadExt;
use futures::AsyncWriteExt;
use futures::SinkExt;
use futures::StreamExt;
use model::olivia::BitMexPriceEventId;
use model::ContractSymbol;
use model::Contracts;
use model::FundingRate;
use model::Leverage;
use model::OfferId;
use model::OpeningFee;
use model::Position;
use model::Price;
use model::Timestamp;
use model::TxFeeRate;
use serde::Deserialize;
use serde::Serialize;
use std::fmt;
use time::Duration;

pub(crate) async fn send<S>(sink: S, offers: Offers) -> Result<(), JsonCodecError>
where
    S: AsyncWriteExt + Unpin,
{
    let mut framed = FramedWrite::new(sink, JsonCodec::<Offers, ()>::new());
    framed.send(offers).await?;
    MESSAGES_SENT.inc();

    Ok(())
}

pub(crate) async fn recv<S>(stream: S) -> Result<Offers, ReceiveError>
where
    S: AsyncReadExt + Unpin,
{
    let mut framed = FramedRead::new(stream, JsonCodec::<(), Offers>::new());

    let offers = framed.next().await.ok_or(ReceiveError::Terminated)??;

    MESSAGES_RECEIVED.inc();

    Ok(offers)
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub(crate) struct Offers(Vec<Offer>);

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Offer {
    id: OfferId,
    contract_symbol: ContractSymbol,
    position_maker: Position,
    price: Price,
    min_quantity: Contracts,
    max_quantity: Contracts,
    leverage_choices: Vec<Leverage>,
    creation_timestamp_maker: Timestamp,
    settlement_interval: Duration,
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
            leverage_choices: offer.leverage_choices,
            creation_timestamp_maker: offer.creation_timestamp_maker,
            settlement_interval: offer.settlement_interval,
            oracle_event_id: offer.oracle_event_id,
            tx_fee_rate: offer.tx_fee_rate,
            funding_rate: offer.funding_rate,
            opening_fee: offer.opening_fee,
        }
    }
}

impl From<Offer> for model::Offer {
    fn from(offer: Offer) -> Self {
        Self {
            id: offer.id,
            contract_symbol: offer.contract_symbol,
            position_maker: offer.position_maker,
            price: offer.price,
            min_quantity: offer.min_quantity,
            max_quantity: offer.max_quantity,
            leverage_choices: offer.leverage_choices,
            creation_timestamp_maker: offer.creation_timestamp_maker,
            settlement_interval: offer.settlement_interval,
            oracle_event_id: offer.oracle_event_id,
            tx_fee_rate: offer.tx_fee_rate,
            funding_rate: offer.funding_rate,
            opening_fee: offer.opening_fee,
        }
    }
}

impl From<Vec<model::Offer>> for Offers {
    fn from(offers: Vec<model::Offer>) -> Self {
        Offers(offers.into_iter().map(Offer::from).collect())
    }
}

impl From<Offers> for Vec<model::Offer> {
    fn from(offers: Offers) -> Self {
        offers.0.into_iter().map(model::Offer::from).collect()
    }
}

impl fmt::Debug for Offer {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("Offer")
            .field("contract_symbol", &self.contract_symbol)
            .field("position_maker", &self.position_maker)
            .field("offer_id", &self.id)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReceiveError {
    #[error("The stream has terminated")]
    Terminated,
    #[error("Failed to decode maker offers")]
    Decode(#[from] JsonCodecError),
}

static MESSAGES_SENT: conquer_once::Lazy<prometheus::IntCounter> = conquer_once::Lazy::new(|| {
    prometheus::register_int_counter!(
        "offer_messages_sent_total_v2",
        "The number of offer messages sent over the libp2p connection.",
    )
    .unwrap()
});

static MESSAGES_RECEIVED: conquer_once::Lazy<prometheus::IntCounter> =
    conquer_once::Lazy::new(|| {
        prometheus::register_int_counter!(
            "offer_messages_received_total",
            "The number of offer messages received over the libp2p connection.",
        )
        .unwrap()
    });

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::dummy_offers;
    use sluice::pipe::pipe;

    #[tokio::test]
    async fn sent_offers_match_received_offers() {
        let (stream, sink) = pipe();

        let maker_offers = dummy_offers();

        let (send_res, recv_res) =
            tokio::join!(send(sink, Offers::from(maker_offers.clone())), recv(stream));

        assert!(send_res.is_ok());
        assert_eq!(maker_offers, Vec::<model::Offer>::from(recv_res.unwrap()))
    }
}
