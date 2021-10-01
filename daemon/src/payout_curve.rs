use crate::model::{Leverage, Usd};
use anyhow::Result;
use bdk::bitcoin;
use cfd_protocol::interval::MAX_PRICE_DEC;
use cfd_protocol::Payout;

pub fn calculate(
    price: Usd,
    _quantity: Usd,
    maker_payin: bitcoin::Amount,
    (taker_payin, _leverage): (bitcoin::Amount, Leverage),
) -> Result<Vec<Payout>> {
    let dollars = price.try_into_u64()?;
    let payouts = vec![
        Payout::new(
            0..=(dollars - 10),
            maker_payin + taker_payin,
            bitcoin::Amount::ZERO,
        )?,
        Payout::new((dollars - 10)..=(dollars + 10), maker_payin, taker_payin)?,
        Payout::new(
            (dollars + 10)..=MAX_PRICE_DEC,
            bitcoin::Amount::ZERO,
            maker_payin + taker_payin,
        )?,
    ]
    .concat();

    Ok(payouts)
}
