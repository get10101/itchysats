use crate::FeeAccount;
use crate::FundingFee;
use crate::Leverage;
use crate::Price;
use crate::TxFeeRate;
use crate::Usd;

#[derive(Debug, Clone, Copy)]
pub struct RolloverParams {
    pub price: Price,
    pub quantity: Usd,
    /// The long leverage
    ///
    /// This is used for calculating the payout curve only, which always requires the long leverage
    /// at the moment
    pub long_leverage: Leverage,
    pub refund_timelock: u32,
    pub fee_rate: TxFeeRate,
    pub fee_account: FeeAccount,
    pub current_fee: FundingFee,
}

impl RolloverParams {
    pub fn new(
        price: Price,
        quantity: Usd,
        long_leverage: Leverage,
        refund_timelock: u32,
        fee_rate: TxFeeRate,
        fee_account: FeeAccount,
        current_fee: FundingFee,
    ) -> Self {
        Self {
            price,
            quantity,
            long_leverage,
            refund_timelock,
            fee_rate,
            fee_account,
            current_fee,
        }
    }

    pub fn funding_fee(&self) -> &FundingFee {
        &self.current_fee
    }
}
