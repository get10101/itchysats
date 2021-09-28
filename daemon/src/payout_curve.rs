use crate::model::{Leverage, Usd};
use anyhow::Result;
use bdk::bitcoin;
use cfd_protocol::Payout;

pub fn calculate(
    _price: Usd,
    _quantity: Usd,
    _maker_payin: bitcoin::Amount,
    (_taker_payin, _leverage): (bitcoin::Amount, Leverage),
) -> Result<Vec<Payout>> {
    Ok(vec![])
}
