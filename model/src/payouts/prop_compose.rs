use crate::CompleteFee;
use crate::Leverage;
use crate::Price;
use crate::Usd;
use bdk::bitcoin::Amount;
use num::FromPrimitive;
use proptest::prop_compose;
use rust_decimal::Decimal;

prop_compose! {
    pub fn arb_price(min: f64, max: f64)(price in min..max) -> Price {
        let price = Decimal::from_f64(price).unwrap();

        Price::new(price).unwrap()
    }
}

#[cfg(test)]
prop_compose! {
    pub fn arb_contracts(min: u64, max: u64)(contracts in min..max) -> Usd {
        let contracts = Decimal::from(contracts);

        Usd::new(contracts)
    }
}

#[cfg(test)]
prop_compose! {
    pub fn arb_leverage(min: u8, max: u8)(leverage in min..max) -> Leverage {
        Leverage::new(leverage).unwrap()
    }
}

#[cfg(test)]
prop_compose! {
    /// Generate an arbitrary fee flow value, between the `lower`
    /// and `upper` bounds.
    ///
    /// A positive value represents a fee flow from long to short.
    /// Conversely, a negative valure represents a fee flow from
    /// short to long.
    pub fn arb_fee_flow(lower: i64, upper: i64)(fee in lower..upper) -> CompleteFee {
        let fee_amount = Amount::from_sat(fee.abs().try_into().unwrap());
        if fee.is_positive() {
            CompleteFee::LongPaysShort(fee_amount)
        } else if fee.is_negative() {
            CompleteFee::ShortPaysLong(fee_amount)
        } else {
            CompleteFee::None
        }
    }
}
