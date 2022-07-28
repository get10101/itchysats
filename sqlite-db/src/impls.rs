use model::CfdEvent;

impl crate::CfdAggregate for model::Cfd {
    type CtorArgs = ();

    fn new(
        _: Self::CtorArgs,
        crate::Cfd {
            id,
            offer_id,
            position,
            initial_price,
            taker_leverage: leverage,
            settlement_interval,
            counterparty_network_identity,
            counterparty_peer_id,
            role,
            quantity_usd,
            opening_fee,
            initial_funding_rate,
            initial_tx_fee_rate,
            trading_pair,
        }: crate::Cfd,
    ) -> Self {
        model::Cfd::new(
            id,
            offer_id,
            position,
            initial_price,
            leverage,
            settlement_interval,
            role,
            quantity_usd,
            counterparty_network_identity,
            counterparty_peer_id,
            opening_fee,
            initial_funding_rate,
            initial_tx_fee_rate,
            trading_pair,
        )
    }

    fn apply(self, event: CfdEvent) -> Self {
        self.apply(event)
    }

    fn version(&self) -> u32 {
        self.version()
    }
}
