use crate::collab_settlement;
use crate::command;
use crate::identify;
use crate::oracle;
use crate::order;
use ping_pong::pong;
use std::collections::HashSet;
use xtra::message_channel::MessageChannel;
use xtra::Address;
use xtra_libp2p::NewInboundSubstream;

pub const MAKER_LISTEN_PROTOCOLS: MakerListenProtocols = MakerListenProtocols::new(
    ping_pong::PROTOCOL,
    identify::PROTOCOL,
    order::PROTOCOL,
    rollover::PROTOCOL,
    rollover::deprecated::PROTOCOL,
    collab_settlement::PROTOCOL,
);

pub const TAKER_LISTEN_PROTOCOLS: TakerListenProtocols = TakerListenProtocols::new(
    ping_pong::PROTOCOL,
    identify::PROTOCOL,
    xtra_libp2p_offer::PROTOCOL,
);

pub const REQUIRED_MAKER_LISTEN_PROTOCOLS: RequiredMakerListenProtocols =
    RequiredMakerListenProtocols::new(
        ping_pong::PROTOCOL,
        identify::PROTOCOL,
        order::PROTOCOL,
        rollover::PROTOCOL,
        collab_settlement::PROTOCOL,
    );

/// Verify if the listen protocols that the `maker` supports are
/// sufficient to fulfil the `requirements` of the taker.
pub fn does_maker_satisfy_taker_needs(
    maker: &HashSet<String>,
    requirements: RequiredMakerListenProtocols,
) -> Result<(), HashSet<String>> {
    // missing protocols are those that are in requirements but not in maker protocols
    let missing_protocols = &HashSet::<String>::from(requirements)
        .difference(maker)
        .cloned()
        .collect::<HashSet<_>>();

    if !missing_protocols.is_empty() {
        return Err(missing_protocols.clone());
    }

    Ok(())
}

/// The set of protocols that the maker's `Endpoint` is listening for.
#[derive(Clone, Copy)]
pub struct MakerListenProtocols {
    ping: &'static str,
    identify: &'static str,
    order: &'static str,
    rollover: &'static str,
    rollover_deprecated: &'static str,
    collaborative_settlement: &'static str,
}

impl MakerListenProtocols {
    pub const NR_OF_SUPPORTED_PROTOCOLS: usize = 6;

    pub const fn new(
        ping: &'static str,
        identify: &'static str,
        order: &'static str,
        rollover: &'static str,
        rollover_deprecated: &'static str,
        collaborative_settlement: &'static str,
    ) -> Self {
        Self {
            ping,
            identify,
            order,
            rollover,
            rollover_deprecated,
            collaborative_settlement,
        }
    }

    /// Construct a map of protocol identifiers to actor addresses.
    ///
    /// This is used so that the `Endpoint` knows who to delegate to
    /// when receiving new inbound substreams.
    pub fn inbound_substream_handlers<R, RD>(
        &self,
        ping_handler: Address<pong::Actor>,
        identify_handler: Address<identify::listener::Actor>,
        order_handler: Address<order::maker::Actor>,
        rollover_handler: Address<
            rollover::maker::Actor<command::Executor, oracle::AnnouncementsChannel, R>,
        >,
        rollover_deprecated_handler: Address<
            rollover::deprecated::maker::Actor<command::Executor, oracle::AnnouncementsChannel, RD>,
        >,
        collaborative_settlement_handler: Address<collab_settlement::maker::Actor>,
    ) -> [(&'static str, MessageChannel<NewInboundSubstream, ()>); Self::NR_OF_SUPPORTED_PROTOCOLS]
    where
        R: rollover::protocol::GetRates + Send + Sync + Clone + 'static,
        RD: rollover::deprecated::protocol::GetRates + Send + Sync + Clone + 'static,
    {
        // We deconstruct to ensure that all protocols are being used
        let MakerListenProtocols {
            ping,
            identify,
            order,
            rollover,
            rollover_deprecated,
            collaborative_settlement,
        } = self;

        [
            (ping, ping_handler.into()),
            (identify, identify_handler.into()),
            (order, order_handler.into()),
            (rollover, rollover_handler.into()),
            (rollover_deprecated, rollover_deprecated_handler.into()),
            (
                collaborative_settlement,
                collaborative_settlement_handler.into(),
            ),
        ]
    }
}

impl From<MakerListenProtocols> for HashSet<String> {
    fn from(maker: MakerListenProtocols) -> Self {
        // We deconstruct to ensure that all protocols are being used
        let MakerListenProtocols {
            ping,
            identify,
            order,
            rollover,
            rollover_deprecated,
            collaborative_settlement,
        } = maker;

        HashSet::from([
            ping.to_string(),
            identify.to_string(),
            order.to_string(),
            rollover.to_string(),
            rollover_deprecated.to_string(),
            collaborative_settlement.to_string(),
        ])
    }
}

/// The set of protocols that the maker's `Endpoint` is expected to
/// be listening for.
#[derive(Clone, Copy)]
pub struct RequiredMakerListenProtocols {
    ping: &'static str,
    identify: &'static str,
    order: &'static str,
    rollover: &'static str,
    collaborative_settlement: &'static str,
}

impl RequiredMakerListenProtocols {
    pub const fn new(
        ping: &'static str,
        identify: &'static str,
        order: &'static str,
        rollover: &'static str,
        collaborative_settlement: &'static str,
    ) -> Self {
        Self {
            ping,
            identify,
            order,
            rollover,
            collaborative_settlement,
        }
    }
}

impl From<RequiredMakerListenProtocols> for HashSet<String> {
    fn from(required: RequiredMakerListenProtocols) -> Self {
        // We deconstruct to ensure that all protocols are being used
        let RequiredMakerListenProtocols {
            ping,
            identify,
            order,
            rollover,
            collaborative_settlement,
        } = required;

        HashSet::from([
            ping.to_string(),
            identify.to_string(),
            order.to_string(),
            rollover.to_string(),
            collaborative_settlement.to_string(),
        ])
    }
}

/// The set of protocols that the taker's `Endpoint` is listening for.
#[derive(Clone, Copy)]
pub struct TakerListenProtocols {
    ping: &'static str,
    identify: &'static str,
    offer: &'static str,
}

impl TakerListenProtocols {
    const NR_OF_SUPPORTED_PROTOCOLS: usize = 3;

    pub const fn new(ping: &'static str, identify: &'static str, offer: &'static str) -> Self {
        Self {
            ping,
            identify,
            offer,
        }
    }

    /// Construct a map of protocol identifiers to actor addresses.
    ///
    /// This is used so that the `Endpoint` knows who to delegate to
    /// when receiving new inbound substreams.
    pub fn inbound_substream_handlers(
        &self,
        ping_handler: Address<pong::Actor>,
        identify_handler: Address<identify::listener::Actor>,
        offer_handler: Address<xtra_libp2p_offer::taker::Actor>,
    ) -> [(&'static str, MessageChannel<NewInboundSubstream, ()>); Self::NR_OF_SUPPORTED_PROTOCOLS]
    {
        // We deconstruct to ensure that all protocols are being used
        let TakerListenProtocols {
            ping,
            identify,
            offer,
        } = self;

        [
            (ping, ping_handler.into()),
            (identify, identify_handler.into()),
            (offer, offer_handler.into()),
        ]
    }
}

impl From<TakerListenProtocols> for HashSet<String> {
    fn from(protocols: TakerListenProtocols) -> Self {
        // We deconstruct to ensure that all protocols are being used
        let TakerListenProtocols {
            ping,
            identify,
            offer,
        } = protocols;

        HashSet::from_iter([ping.to_string(), identify.to_string(), offer.to_string()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_required_taker_protocols_are_supported_by_maker() {
        let result = does_maker_satisfy_taker_needs(
            &MAKER_LISTEN_PROTOCOLS.into(),
            REQUIRED_MAKER_LISTEN_PROTOCOLS,
        );

        assert!(result.is_ok(), "Missing protocols detected: {result:?}");
    }

    #[test]
    fn given_maker_maker_does_not_support_required_protocol_then_error_with_protocol_diff() {
        let mut maker_protocols_as_hashset: HashSet<String> = MAKER_LISTEN_PROTOCOLS.into();
        // remove the ping protocol, we assume that it always is in there
        maker_protocols_as_hashset.remove(ping_pong::PROTOCOL);

        let err = does_maker_satisfy_taker_needs(
            &maker_protocols_as_hashset,
            REQUIRED_MAKER_LISTEN_PROTOCOLS,
        )
        .unwrap_err();

        assert_eq!(err, HashSet::from([ping_pong::PROTOCOL.to_string()]))
    }

    #[test]
    fn given_maker_maker_supports_more_protocols_than_required_then_ok() {
        let mut maker_protocols_as_hashset: HashSet<String> = MAKER_LISTEN_PROTOCOLS.into();
        // remove the ping protocol, we assume that it always is in there
        maker_protocols_as_hashset.insert("blablubb".to_string());

        let result = does_maker_satisfy_taker_needs(
            &maker_protocols_as_hashset,
            REQUIRED_MAKER_LISTEN_PROTOCOLS,
        );

        assert!(result.is_ok(), "Missing protocols detected: {result:?}");
    }

    #[test]
    fn ensure_nr_of_maker_protocols_matches_hashset_len() {
        let maker_protocols_as_hashset: HashSet<String> = MAKER_LISTEN_PROTOCOLS.into();
        assert_eq!(
            MakerListenProtocols::NR_OF_SUPPORTED_PROTOCOLS,
            maker_protocols_as_hashset.len()
        );
    }

    #[test]
    fn ensure_nr_of_taker_protocols_matches_hashset_len() {
        let taker_protocols_as_hashset: HashSet<String> = TAKER_LISTEN_PROTOCOLS.into();
        assert_eq!(
            TakerListenProtocols::NR_OF_SUPPORTED_PROTOCOLS,
            taker_protocols_as_hashset.len()
        );
    }
}
