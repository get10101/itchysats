use crate::Contracts;
use crate::FeeAccount;
use crate::Identity;
use crate::Leverage;
use crate::Price;
use crate::TxFeeRate;
use anyhow::Result;
use bdk::bitcoin::Amount;

#[derive(Clone, Copy, Debug)]
pub struct SetupParams {
    pub margin: Amount,
    pub counterparty_margin: Amount,
    pub counterparty_identity: Identity,
    pub price: Price,
    pub quantity: Contracts,
    pub long_leverage: Leverage,
    pub short_leverage: Leverage,
    pub refund_timelock: u32,
    pub tx_fee_rate: TxFeeRate,
    pub fee_account: FeeAccount,
}

impl SetupParams {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        margin: Amount,
        counterparty_margin: Amount,
        counterparty_identity: Identity,
        price: Price,
        quantity: Contracts,
        long_leverage: Leverage,
        short_leverage: Leverage,
        refund_timelock: u32,
        tx_fee_rate: TxFeeRate,
        fee_account: FeeAccount,
    ) -> Result<Self> {
        Ok(Self {
            margin,
            counterparty_margin,
            counterparty_identity,
            price,
            quantity,
            long_leverage,
            short_leverage,
            refund_timelock,
            tx_fee_rate,
            fee_account,
        })
    }

    pub fn counterparty_identity(&self) -> Identity {
        self.counterparty_identity
    }
}
