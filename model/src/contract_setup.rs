use crate::FeeAccount;
use crate::Identity;
use crate::Leverage;
use crate::Price;
use crate::TxFeeRate;
use crate::Usd;
use anyhow::Result;
use bdk::bitcoin::Amount;

#[derive(Clone, Copy)]
pub struct SetupParams {
    pub margin: Amount,
    pub counterparty_margin: Amount,
    pub counterparty_identity: Identity,
    pub price: Price,
    pub quantity: Usd,
    /// The long leverage
    ///
    /// This is used for calculating the payout curve only, which always requires the long leverage
    /// at the moment
    pub long_leverage: Leverage,
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
        quantity: Usd,
        long_leverage: Leverage,
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
            refund_timelock,
            tx_fee_rate,
            fee_account,
        })
    }

    pub fn counterparty_identity(&self) -> Identity {
        self.counterparty_identity
    }
}
