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
    pub long_leverage: Leverage,
    pub short_leverage: Leverage,
    pub refund_timelock: u32,
    pub fee_rate: TxFeeRate,
    pub fee_account: FeeAccount,
    pub current_fee: FundingFee,
}

impl RolloverParams {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        price: Price,
        quantity: Usd,
        long_leverage: Leverage,
        short_leverage: Leverage,
        refund_timelock: u32,
        fee_rate: TxFeeRate,
        fee_account: FeeAccount,
        current_fee: FundingFee,
    ) -> Self {
        Self {
            price,
            quantity,
            long_leverage,
            short_leverage,
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
