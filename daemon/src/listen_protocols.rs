use crate::collab_settlement;
use crate::command;
use crate::identify;
use crate::oracle;
use crate::order;
use std::collections::HashSet;
use xtra::message_channel::MessageChannel;
use xtra::Address;
use xtra_libp2p::NewInboundSubstream;
use xtra_libp2p_ping::pong;

pub const MAKER_LISTEN_PROTOCOLS: MakerListenProtocols = MakerListenProtocols::new(
    xtra_libp2p_ping::PROTOCOL,
    identify::PROTOCOL,
    order::PROTOCOL,
    rollover::deprecated::PROTOCOL,
    rollover::PROTOCOL,
    collab_settlement::PROTOCOL,
);

pub const TAKER_LISTEN_PROTOCOLS: TakerListenProtocols = TakerListenProtocols::new(
    xtra_libp2p_ping::PROTOCOL,
    identify::PROTOCOL,
    xtra_libp2p_offer::PROTOCOL,
);

pub const REQUIRED_MAKER_LISTEN_PROTOCOLS: RequiredMakerListenProtocols =
    RequiredMakerListenProtocols::new(
        xtra_libp2p_ping::PROTOCOL,
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
    ping_v1: &'static str,
    identify_v1: &'static str,
    order_v1: &'static str,
    rollover_deprecated: &'static str,
    rollover: &'static str,
    collaborative_settlement_v1: &'static str,
}

impl MakerListenProtocols {
    pub const NR_OF_SUPPORTED_PROTOCOLS: usize = 6;

    pub const fn new(
        ping_v1: &'static str,
        identify_v1: &'static str,
        order_v1: &'static str,
        rollover_deprecated: &'static str,
        rollover: &'static str,
        collaborative_settlement_v1: &'static str,
    ) -> Self {
        Self {
            ping_v1,
            identify_v1,
            order_v1,
            rollover_deprecated,
            rollover,
            collaborative_settlement_v1,
        }
    }

    /// Construct a map of protocol identifiers to actor addresses.
    ///
    /// This is used so that the `Endpoint` knows who to delegate to
    /// when receiving new inbound substreams.
    pub fn inbound_substream_handlers<R1, R2>(
        &self,
        ping_v1_handler: Address<pong::Actor>,
        identify_v1_handler: Address<identify::listener::Actor>,
        order_v1_handler: Address<order::maker::Actor>,
        rollover_deprecated_handler: Address<
            rollover::deprecated::maker::Actor<command::Executor, oracle::AnnouncementsChannel, R1>,
        >,
        rollover_handler: Address<
            rollover::maker::Actor<command::Executor, oracle::AnnouncementsChannel, R2>,
        >,
        collaborative_settlement_v1_handler: Address<collab_settlement::maker::Actor>,
    ) -> [(&'static str, MessageChannel<NewInboundSubstream, ()>); Self::NR_OF_SUPPORTED_PROTOCOLS]
    where
        R1: rollover::deprecated::protocol::GetRates + Send + Sync + Clone + 'static,
        R2: rollover::protocol::GetRates + Send + Sync + Clone + 'static,
    {
        // We deconstruct to ensure that all protocols are being used
        let MakerListenProtocols {
            ping_v1,
            identify_v1,
            order_v1,
            rollover_deprecated,
            rollover,
            collaborative_settlement_v1,
        } = self;

        [
            (ping_v1, ping_v1_handler.into()),
            (identify_v1, identify_v1_handler.into()),
            (order_v1, order_v1_handler.into()),
            (rollover_deprecated, rollover_deprecated_handler.into()),
            (rollover, rollover_handler.into()),
            (
                collaborative_settlement_v1,
                collaborative_settlement_v1_handler.into(),
            ),
        ]
    }
}

impl From<MakerListenProtocols> for HashSet<String> {
    fn from(maker: MakerListenProtocols) -> Self {
        // We deconstruct to ensure that all protocols are being used
        let MakerListenProtocols {
            ping_v1,
            identify_v1,
            order_v1,
            rollover_deprecated,
            rollover,
            collaborative_settlement_v1,
        } = maker;

        HashSet::from([
            ping_v1.to_string(),
            identify_v1.to_string(),
            order_v1.to_string(),
            rollover_deprecated.to_string(),
            rollover.to_string(),
            collaborative_settlement_v1.to_string(),
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
    ping_v1: &'static str,
    identify_v1: &'static str,
    offer_v1: &'static str,
}

impl TakerListenProtocols {
    const NR_OF_SUPPORTED_PROTOCOLS: usize = 3;

    pub const fn new(
        ping_v1: &'static str,
        identify_v1: &'static str,
        offer_v1: &'static str,
    ) -> Self {
        Self {
            ping_v1,
            identify_v1,
            offer_v1,
        }
    }

    /// Construct a map of protocol identifiers to actor addresses.
    ///
    /// This is used so that the `Endpoint` knows who to delegate to
    /// when receiving new inbound substreams.
    pub fn inbound_substream_handlers(
        &self,
        ping_v1_handler: Address<pong::Actor>,
        identify_v1_handler: Address<identify::listener::Actor>,
        offer_v1_handler: Address<xtra_libp2p_offer::taker::Actor>,
    ) -> [(&'static str, MessageChannel<NewInboundSubstream, ()>); Self::NR_OF_SUPPORTED_PROTOCOLS]
    {
        // We deconstruct to ensure that all protocols are being used
        let TakerListenProtocols {
            ping_v1,
            identify_v1,
            offer_v1,
        } = self;

        [
            (ping_v1, ping_v1_handler.into()),
            (identify_v1, identify_v1_handler.into()),
            (offer_v1, offer_v1_handler.into()),
        ]
    }
}

impl From<TakerListenProtocols> for HashSet<String> {
    fn from(protocols: TakerListenProtocols) -> Self {
        // We deconstruct to ensure that all protocols are being used
        let TakerListenProtocols {
            ping_v1,
            identify_v1,
            offer_v1,
        } = protocols;

        HashSet::from_iter([
            ping_v1.to_string(),
            identify_v1.to_string(),
            offer_v1.to_string(),
        ])
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
        maker_protocols_as_hashset.remove(xtra_libp2p_ping::PROTOCOL);

        let err = does_maker_satisfy_taker_needs(
            &maker_protocols_as_hashset,
            REQUIRED_MAKER_LISTEN_PROTOCOLS,
        )
        .unwrap_err();

        assert_eq!(err, HashSet::from([xtra_libp2p_ping::PROTOCOL.to_string()]))
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
