use crate::Contracts;
use crate::FeeAccount;
use crate::Leverage;
use crate::Percent;
use crate::Position;
use crate::Price;
use anyhow::Context;
use anyhow::Result;
use bdk::bitcoin::Amount;
use bdk::bitcoin::SignedAmount;
use num::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

mod implementation;

pub use implementation::calculate;

/// Calculates the margin in BTC
///
/// The initial margin represents the collateral both parties have to come up with
/// to satisfy the contract.
pub(crate) fn calculate_margin(price: Price, quantity: Contracts, leverage: Leverage) -> Amount {
    quantity / (price * leverage)
}

/// Compute the PNL for the given `payout` and `margin` amounts.
///
/// The PNL is returned as a `bitcoin::SignedAmount` and as a percentage of the original margin.
pub fn calculate_profit(payout: Amount, margin: Amount) -> (SignedAmount, Percent) {
    let payout = payout
        .to_signed()
        .expect("amount to fit into signed amount");
    let margin = margin
        .to_signed()
        .expect("amount to fit into signed amount");

    let profit = payout - margin;

    let profit_sats = Decimal::from(profit.as_sat());
    let margin_sats = Decimal::from(margin.as_sat());
    let percent = dec!(100) * profit_sats / margin_sats;

    (profit, Percent(percent))
}

/// Compute the payout for the given CFD parameters at a particular `closing_price`.
///
/// The PNL, both as a `bitcoin::SignedAmount` and as a percentage, is also returned for
/// convenience.
///
/// The `Position` is determined based on the `FeeAccount`.
///
/// These formulas are independent of the inverse payout curve implementation and are therefore
/// theoretical. There could be slight differences between what we return here and what the payout
/// curve determines.
pub fn calculate_payout_at_price(
    opening_price: Price,
    closing_price: Price,
    quantity: Contracts,
    long_leverage: Leverage,
    short_leverage: Leverage,
    fee_account: FeeAccount,
) -> Result<(Amount, SignedAmount, Percent)> {
    let long_margin = calculate_margin(opening_price, quantity, long_leverage);
    let short_margin = calculate_margin(opening_price, quantity, short_leverage);
    let total_margin = long_margin + short_margin;

    let uncapped_pnl_long = {
        let opening_price = opening_price.0;
        let closing_price = closing_price.0;
        let quantity = quantity.0;

        let uncapped_pnl = (quantity / opening_price) - (quantity / closing_price);
        let uncapped_pnl = uncapped_pnl
            .round_dp_with_strategy(8, rust_decimal::RoundingStrategy::MidpointAwayFromZero);
        let uncapped_pnl = uncapped_pnl
            .to_f64()
            .context("Could not convert Decimal to f64")?;

        SignedAmount::from_btc(uncapped_pnl)?
    };

    let position = fee_account.position;
    let fee_offset = fee_account.balance();
    let (margin, payout) = match position {
        Position::Long => {
            let payout = {
                let long_margin = long_margin
                    .to_signed()
                    .context("Unable to compute long margin")?;

                long_margin - fee_offset + uncapped_pnl_long
            };

            (long_margin, payout)
        }
        Position::Short => {
            let payout = {
                let short_margin = short_margin
                    .to_signed()
                    .context("Unable to compute short margin")?;

                short_margin - fee_offset - uncapped_pnl_long
            };

            (short_margin, payout)
        }
    };

    let payout = payout.to_unsigned().unwrap_or(Amount::ZERO);
    let payout = payout.min(total_margin);

    let (profit_btc, profit_percent) = calculate_profit(payout, margin);

    Ok((payout, profit_btc, profit_percent))
}
