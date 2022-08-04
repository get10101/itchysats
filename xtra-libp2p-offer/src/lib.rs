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
use std::fmt;
use time::Duration;

pub mod maker;
mod protocol;
pub mod taker;

pub const PROTOCOL: &str = "/itchysats/offer/1.0.0";

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct MakerOffers {
    pub long: Option<Offer>,
    pub short: Option<Offer>,
    pub tx_fee_rate: TxFeeRate,
    pub funding_rate_long: FundingRate,
    pub funding_rate_short: FundingRate,
}

impl fmt::Debug for MakerOffers {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("MakerOffers")
            .field("long_order_id", &self.long.as_ref().map(|o| o.id))
            .field("short_order_id", &self.short.as_ref().map(|o| o.id))
            .field("tx_fee_rate", &self.tx_fee_rate)
            .field("funding_rate_long", &self.funding_rate_long)
            .field("funding_rate_short", &self.funding_rate_short)
            .finish()
    }
}

/// A concrete order created by a maker for a taker
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Offer {
    pub id: OfferId,

    #[serde(rename = "trading_pair")]
    pub contract_symbol: ContractSymbol,

    /// The maker's position
    ///
    /// Since the maker is the creator of this order this always reflects the maker's position.
    /// When we create a Cfd we change it to be the position as seen by each party, i.e. flip the
    /// position for the taker.
    #[serde(rename = "position")]
    pub position_maker: Position,

    pub price: Price,

    pub min_quantity: Usd,
    pub max_quantity: Usd,

    /// A selection of leverages that the maker allows for the taker
    pub leverage_choices: Vec<Leverage>,

    /// The creation timestamp as set by the maker
    #[serde(rename = "creation_timestamp")]
    pub creation_timestamp_maker: Timestamp,

    /// The duration that will be used for calculating the settlement timestamp
    pub settlement_interval: Duration,

    pub origin: Origin,

    /// The id of the event to be used for price attestation
    ///
    /// The maker includes this into the Order based on the Oracle announcement to be used.
    pub oracle_event_id: BitMexPriceEventId,

    pub tx_fee_rate: TxFeeRate,
    pub funding_rate: FundingRate,
    pub opening_fee: OpeningFee,
}

impl From<MakerOffers> for model::MakerOffers {
    fn from(maker_offers: MakerOffers) -> Self {
        model::MakerOffers {
            long: maker_offers.long.map(|offer| offer.into()),
            short: maker_offers.short.map(|offer| offer.into()),
            tx_fee_rate: maker_offers.tx_fee_rate,
            funding_rate_long: maker_offers.funding_rate_long,
            funding_rate_short: maker_offers.funding_rate_short,
        }
    }
}

#[allow(deprecated)] // allowing for backwards compatible conversion
impl From<Offer> for model::Offer {
    fn from(offer: Offer) -> Self {
        model::Offer {
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
            origin: offer.origin,
            oracle_event_id: offer.oracle_event_id,
            tx_fee_rate: offer.tx_fee_rate,
            funding_rate: offer.funding_rate,
            opening_fee: offer.opening_fee,
        }
    }
}

impl From<model::MakerOffers> for MakerOffers {
    fn from(maker_offers: model::MakerOffers) -> Self {
        MakerOffers {
            long: maker_offers.long.map(|offer| offer.into()),
            short: maker_offers.short.map(|offer| offer.into()),
            tx_fee_rate: maker_offers.tx_fee_rate,
            funding_rate_long: maker_offers.funding_rate_long,
            funding_rate_short: maker_offers.funding_rate_short,
        }
    }
}

impl From<model::Offer> for Offer {
    fn from(offer: model::Offer) -> Self {
        Offer {
            id: offer.id,
            contract_symbol: offer.contract_symbol,
            position_maker: offer.position_maker,
            price: offer.price,
            min_quantity: offer.min_quantity,
            max_quantity: offer.max_quantity,
            leverage_choices: offer.leverage_choices,
            creation_timestamp_maker: offer.creation_timestamp_maker,
            settlement_interval: offer.settlement_interval,
            origin: offer.origin,
            oracle_event_id: offer.oracle_event_id,
            tx_fee_rate: offer.tx_fee_rate,
            funding_rate: offer.funding_rate,
            opening_fee: offer.opening_fee,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::taker::LatestMakerOffers;
    use async_trait::async_trait;
    use futures::Future;
    use futures::FutureExt;
    use model::olivia::BitMexPriceEventId;
    use model::ContractSymbol;
    use model::FundingRate;
    use model::Leverage;
    use model::Origin;
    use model::Position;
    use model::Price;
    use model::TxFeeRate;
    use model::Usd;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::time::Duration;
    use time::macros::datetime;
    use xtra::spawn::TokioGlobalSpawnExt;
    use xtra::Actor as _;
    use xtra::Address;
    use xtra::Context;
    use xtra_libp2p::endpoint::Subscribers;
    use xtra_libp2p::libp2p::identity::Keypair;
    use xtra_libp2p::libp2p::multiaddr::Protocol;
    use xtra_libp2p::libp2p::transport::MemoryTransport;
    use xtra_libp2p::libp2p::Multiaddr;
    use xtra_libp2p::libp2p::PeerId;
    use xtra_libp2p::Connect;
    use xtra_libp2p::Endpoint;
    use xtra_libp2p::ListenOn;
    use xtra_productivity::xtra_productivity;

    #[tokio::test]
    async fn given_new_offers_then_received_offers_match_originals() {
        tracing_subscriber::fmt()
            .with_env_filter("xtra_libp2p_offer=trace")
            .with_test_writer()
            .init();

        let (maker_peer_id, maker_offer_addr, maker_endpoint_addr) =
            create_endpoint_with_offer_maker();
        let (offer_receiver_addr, taker_endpoint_addr) = create_endpoint_with_offer_taker();

        maker_endpoint_addr
            .send(ListenOn(Multiaddr::empty().with(Protocol::Memory(1000))))
            .await
            .unwrap();
        taker_endpoint_addr
            .send(Connect(
                Multiaddr::empty()
                    .with(Protocol::Memory(1000))
                    .with(Protocol::P2p(maker_peer_id.into())),
            ))
            .await
            .unwrap()
            .unwrap();

        let new_offers = dummy_maker_offers();

        // maker keeps sending the offers until the taker establishes
        // a connection
        #[allow(clippy::disallowed_methods)]
        tokio::spawn({
            let new_offers = new_offers.clone();
            async move {
                loop {
                    maker_offer_addr
                        .send(crate::maker::NewOffers::new(new_offers.clone()))
                        .await
                        .unwrap();

                    tokio_extras::time::sleep(Duration::from_millis(200)).await;
                }
            }
        });

        // taker retries until the connection is established and we
        // get the maker's latest offers
        let received_offers = retry_until_some(|| {
            let offer_receiver_addr = offer_receiver_addr.clone();
            async move {
                offer_receiver_addr
                    .send(GetLatestOffers)
                    .map(|res| res.unwrap())
                    .await
            }
        })
        .await;

        assert_eq!(new_offers, Some(received_offers))
    }

    fn create_endpoint_with_offer_maker(
    ) -> (PeerId, Address<crate::maker::Actor>, Address<Endpoint>) {
        let (endpoint_addr, endpoint_context) = Context::new(None);

        let id = Keypair::generate_ed25519();
        let offer_maker_addr = crate::maker::Actor::new(endpoint_addr.clone())
            .create(None)
            .spawn_global();

        let endpoint = Endpoint::new(
            Box::new(MemoryTransport::default),
            id.clone(),
            Duration::from_secs(10),
            [],
            Subscribers::new(
                vec![offer_maker_addr.clone().into()],
                vec![offer_maker_addr.clone().into()],
                vec![],
                vec![],
            ),
        );

        #[allow(clippy::disallowed_methods)]
        tokio::spawn(endpoint_context.run(endpoint));

        (id.public().to_peer_id(), offer_maker_addr, endpoint_addr)
    }

    fn create_endpoint_with_offer_taker() -> (Address<OffersReceiver>, Address<Endpoint>) {
        let offers_receiver_addr = OffersReceiver::new().create(None).spawn_global();

        let offer_taker_addr = crate::taker::Actor::new(offers_receiver_addr.clone().into())
            .create(None)
            .spawn_global();

        let endpoint_addr = Endpoint::new(
            Box::new(MemoryTransport::default),
            Keypair::generate_ed25519(),
            Duration::from_secs(10),
            [(PROTOCOL, offer_taker_addr.into())],
            Subscribers::default(),
        )
        .create(None)
        .spawn_global();

        (offers_receiver_addr, endpoint_addr)
    }

    struct OffersReceiver {
        latest_offers: Option<MakerOffers>,
    }

    impl OffersReceiver {
        fn new() -> Self {
            Self {
                latest_offers: None,
            }
        }
    }

    #[async_trait]
    impl xtra::Actor for OffersReceiver {
        type Stop = ();

        async fn stopped(self) -> Self::Stop {}
    }

    #[xtra_productivity]
    impl OffersReceiver {
        async fn handle(&mut self, msg: LatestMakerOffers) {
            self.latest_offers = msg.0.map(|offer| offer.into());
        }
    }

    struct GetLatestOffers;

    #[xtra_productivity]
    impl OffersReceiver {
        async fn handle(&mut self, _: GetLatestOffers) -> Option<MakerOffers> {
            self.latest_offers.clone()
        }
    }

    async fn retry_until_some<F, FUT, T>(mut fut: F) -> T
    where
        F: FnMut() -> FUT,
        FUT: Future<Output = Option<T>>,
    {
        loop {
            match fut().await {
                Some(t) => return t,
                None => tokio_extras::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }

    pub fn dummy_maker_offers() -> Option<MakerOffers> {
        Some(MakerOffers {
            long: Some(dummy_offer(Position::Long)),
            short: Some(dummy_offer(Position::Short)),
            tx_fee_rate: TxFeeRate::default(),
            funding_rate_long: FundingRate::new(Decimal::ONE).unwrap(),
            funding_rate_short: FundingRate::new(Decimal::NEGATIVE_ONE).unwrap(),
        })
    }

    fn dummy_offer(position: Position) -> Offer {
        Offer {
            id: Default::default(),
            contract_symbol: ContractSymbol::BtcUsd,
            position_maker: position,
            price: Price::new(dec!(1000)).unwrap(),
            min_quantity: Usd::new(dec!(100)),
            max_quantity: Usd::new(dec!(1000)),
            leverage_choices: vec![Leverage::TWO],
            creation_timestamp_maker: Timestamp::now(),
            settlement_interval: time::Duration::hours(24),
            origin: Origin::Ours,
            oracle_event_id: BitMexPriceEventId::with_20_digits(
                datetime!(2021-10-04 22:00:00).assume_utc(),
            ),
            tx_fee_rate: Default::default(),
            funding_rate: Default::default(),
            opening_fee: Default::default(),
        }
    }
}
