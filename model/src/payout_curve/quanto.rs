use crate::CompleteFee;
use crate::Leverage;
use crate::Position;
use anyhow::Context;
use anyhow::Result;
use bdk::bitcoin::Amount;
use bdk::bitcoin::SignedAmount;
use num::FromPrimitive;
use num::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;
use std::ops::RangeInclusive;

/// Discretization of a quanto payout curve.
///
/// The number of elements in the underlying vector indicates the number of intervals into which the
/// quanto payout curve was divided.
pub struct Payouts(Vec<Payout>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payout {
    pub interval: RangeInclusive<u64>,
    pub long: Amount,
    pub short: Amount,
}

impl Payouts {
    pub fn new(
        initial_price: u64,
        n_contracts: u64,
        leverage_long: Leverage,
        leverage_short: Leverage,
        n_payouts: usize,
        multiplier: Decimal,
        fee_offset: CompleteFee,
    ) -> Result<Self, Error> {
        let payouts = Curve::new(
            initial_price,
            n_contracts,
            leverage_long,
            leverage_short,
            n_payouts,
            multiplier,
            fee_offset,
        )
        .discretized_payouts()?;

        Ok(Self(payouts))
    }

    pub fn into_inner(self) -> Vec<Payout> {
        self.0
    }
}

/// Model for a quanto payout curve.
struct Curve {
    /// Number of contracts that make up the position.
    n_contracts: u64,
    /// Long position's leverage.
    leverage_long: Leverage,
    /// Short position's leverage.
    leverage_short: Leverage,
    /// Entry price.
    initial_price: u64,

    /// A party's initial margin is offset by this much, depending on their position.
    fee_offset: CompleteFee,

    /// Number of distinct intervals into which the underlying payout curve is discretized.
    n_payouts: usize,

    /// Inherent multiplier based on the `COIN` of the `COINUSD` contract symbol.
    multiplier: Decimal,
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Party going long is in too much fee-debt to set up a contract: fees owed {owing} > margin {margin}")]
    LongOwesTooMuch { owing: Amount, margin: Amount },
    #[error("Party going short is in too much fee-debt to set up a contract: fees owed {owing} > margin {margin}")]
    ShortOwesTooMuch { owing: Amount, margin: Amount },
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

impl Curve {
    pub fn new(
        initial_price: u64,
        n_contracts: u64,
        leverage_long: Leverage,
        leverage_short: Leverage,
        n_payouts: usize,
        multiplier: Decimal,
        fee_offset: CompleteFee,
    ) -> Self {
        Self {
            n_payouts,
            initial_price,
            n_contracts,
            leverage_long,
            leverage_short,
            multiplier,
            fee_offset,
        }
    }

    /// Discretize the payout curve into distinct payouts.
    pub fn discretized_payouts(&self) -> Result<Vec<Payout>, Error> {
        let initial_margin_long = self.initial_margin_long();
        let initial_margin_short = self.initial_margin_short();
        let initial_margin_total = initial_margin_long + initial_margin_short;

        match self.fee_offset {
            CompleteFee::LongPaysShort(long_owes) if long_owes > initial_margin_long => {
                return Err(Error::LongOwesTooMuch {
                    owing: long_owes,
                    margin: initial_margin_long,
                });
            }
            CompleteFee::ShortPaysLong(short_owes) if short_owes > initial_margin_short => {
                return Err(Error::ShortOwesTooMuch {
                    owing: short_owes,
                    margin: initial_margin_short,
                });
            }
            _ => {}
        }

        let long_liquidation_interval = self
            .long_liquidation_interval()
            .context("Could not calculate long liquidation interval")?;
        let mut short_liquidation_interval = self
            .short_liquidation_interval()
            .context("Could not calculate short liquidation interval")?;

        // Under very specific conditions the liquidation intervals can overlap. To avoid this
        // situation we shift the short liquidation interval by 1
        if long_liquidation_interval.end() == short_liquidation_interval.start() {
            short_liquidation_interval =
                *short_liquidation_interval.start() + 1..=*short_liquidation_interval.end()
        }

        let long_liquidation_threshold = long_liquidation_interval.end();
        let short_liquidation_threshold = short_liquidation_interval.start();

        let long_liquidation_payout = Payout {
            interval: long_liquidation_interval.clone(),
            long: Amount::ZERO,
            short: initial_margin_total,
        };

        let mut payouts = vec![long_liquidation_payout];

        // We want to split the settlement region of the curve into `n-payouts - 2` segments
        let step = Decimal::from(short_liquidation_threshold - long_liquidation_threshold)
            / Decimal::from(self.n_payouts - 2);
        let step = step.to_u64().context("Could not convert step to u64")?;

        // We start building the settlement intervals right after the end of the long liquidation
        // interval
        let mut prev_upper_bound = *long_liquidation_threshold;
        loop {
            let lower_bound = prev_upper_bound + 1;
            let upper_bound = lower_bound + step;

            // The settlement region of the curve ends as soon as the sub-interval we are building
            // either touches or overlaps with the start of the short liquidation interval
            if upper_bound >= short_liquidation_threshold - 1 {
                break;
            }

            let interval = lower_bound..=upper_bound;
            let payout = self
                .payout_at_interval(interval.clone())
                .with_context(|| format!("Could not calculate payout at interval {interval:?}"))?;
            payouts.push(payout);

            prev_upper_bound = upper_bound;
        }

        // We have to consider a special case if the last settlement interval is smaller than every
        // other settlement interval
        if prev_upper_bound + 1 < *short_liquidation_threshold {
            let lower_bound = prev_upper_bound + 1;
            let upper_bound = short_liquidation_threshold - 1;

            let interval = lower_bound..=upper_bound;
            let payout = self
                .payout_at_interval(interval.clone())
                .with_context(|| format!("Could not calculate payout at interval {interval:?}"))?;
            payouts.push(payout);
        }

        let short_liquidation_payout = Payout {
            long: initial_margin_total,
            short: Amount::ZERO,
            interval: short_liquidation_interval.clone(),
        };
        payouts.push(short_liquidation_payout);

        Ok(payouts)
    }

    /// Build the `Payout` for an arbitrary interval.
    fn payout_at_interval(&self, interval: RangeInclusive<u64>) -> Result<Payout> {
        let fee_offset_long = self.fee_offset.as_signed_amount(Position::Long);
        let fee_offset_short = self.fee_offset.as_signed_amount(Position::Short);

        // We take the value of the closing price for this interval as the midpoint of the
        // interval
        let midpoint = interval.start() + ((interval.end() - interval.start()) / 2);
        let pnl = self
            .pnl_at_closing_price(midpoint)
            .with_context(|| format!("Could not calculate PNL at price {midpoint}"))?;

        let long = self
            .initial_margin_long()
            .to_signed()
            .context("Could not convert long's initial margin to bitcoin::SignedAmount")?;
        let long = long + fee_offset_long + pnl.long();
        let long = long.to_unsigned().unwrap_or(Amount::ZERO);

        let short = self
            .initial_margin_short()
            .to_signed()
            .context("Could not convert short's initial margin to bitcoin::SignedAmount")?;
        let short = short + fee_offset_short + pnl.short();
        let short = short.to_unsigned().unwrap_or(Amount::ZERO);

        Ok(Payout {
            interval,
            long,
            short,
        })
    }

    /// Price interval at which the party going long gets liquidated i.e. their payout amount equals
    /// zero.
    fn long_liquidation_interval(&self) -> Result<RangeInclusive<u64>> {
        let initial_margin = self
            .initial_margin_long()
            .to_signed()
            .context("Could not convert long's initial margin to bitcoin::SignedAmount")?;
        let effective_initial_margin =
            initial_margin + self.fee_offset.as_signed_amount(Position::Long);
        let effective_initial_margin = effective_initial_margin
            .to_unsigned()
            .context("Could not convert long's effective initial margin to bitcoin::Amount")?;

        let bankruptcy_price = bankruptcy_price_long(
            effective_initial_margin,
            self.n_contracts,
            self.initial_price,
            self.multiplier,
        )
        .context("Could not calculate long's bankruptcy price")?;

        Ok(0..=bankruptcy_price)
    }

    /// Price interval at which the party going short gets liquidated i.e. their payout amount
    /// equals zero.
    fn short_liquidation_interval(&self) -> Result<RangeInclusive<u64>> {
        let initial_margin = self
            .initial_margin_short()
            .to_signed()
            .context("Could not convert short's initial margin to bitcoin::SignedAmount")?;
        let effective_initial_margin =
            initial_margin + self.fee_offset.as_signed_amount(Position::Short);
        let effective_initial_margin = effective_initial_margin
            .to_unsigned()
            .context("Could not convert short's effective initial margin to bitcoin::Amount")?;

        let bankruptcy_price = bankruptcy_price_short(
            effective_initial_margin,
            self.n_contracts,
            self.initial_price,
            self.multiplier,
        )
        .context("Could not calculate short's bankruptcy price")?;

        Ok(bankruptcy_price..=maia_core::interval::MAX_PRICE_DEC)
    }

    /// Compute the initial BTC margin that the party going long has to put up.
    fn initial_margin_long(&self) -> Amount {
        calculate_initial_margin(
            self.initial_price,
            self.n_contracts,
            self.leverage_long,
            self.multiplier,
        )
    }

    /// Compute the initial BTC margin that the party going short has to put up.
    fn initial_margin_short(&self) -> Amount {
        calculate_initial_margin(
            self.initial_price,
            self.n_contracts,
            self.leverage_short,
            self.multiplier,
        )
    }

    /// Compute the profit and loss (PNL) at the given `closing_price`.
    fn pnl_at_closing_price(&self, closing_price: u64) -> Result<Pnl> {
        Pnl::new(
            self.initial_price,
            closing_price,
            self.multiplier,
            self.n_contracts,
        )
    }
}

/// Compute the initial BTC margin that a party has to put up, according to their `leverage`.
fn calculate_initial_margin(
    initial_price: u64,
    n_contracts: u64,
    leverage: Leverage,
    multiplier: Decimal,
) -> Amount {
    let n_contracts = Decimal::from(n_contracts);
    let leverage = Decimal::from(leverage.get());
    let initial_price = Decimal::from(initial_price);

    let margin = (n_contracts * initial_price * multiplier) / leverage;
    let margin = margin.round_dp_with_strategy(8, RoundingStrategy::MidpointAwayFromZero);
    let margin = margin.to_f64().expect("margin to fit into f64");

    Amount::from_btc(margin).expect("margin to fit into bitcoin::Amount")
}

/// The profit and loss (PNL).
///
/// It is convenient to model the calculations for long and short together, because one party's gain
/// is the other one's loss and vice-versa. That is, the absolute value of PNL will be the same,
/// with only the sign changing between the two parties.
struct Pnl(SignedAmount);

impl Pnl {
    /// Compute the PNL of the contract.
    ///
    /// Call `Self::new().long()` and `Self::new().short()` to access the PNL values for long and
    /// short respectively.
    fn new(
        initial_price: u64,
        closing_price: u64,
        multiplier: Decimal,
        n_contracts: u64,
    ) -> Result<Self> {
        let initial_price = Decimal::from(initial_price);
        let closing_price = Decimal::from(closing_price);
        let n_contracts = Decimal::from(n_contracts);

        let pnl = (closing_price - initial_price) * multiplier * n_contracts;
        let pnl = pnl.to_f64().context("Could not convert PNL to f64")?;
        let pnl = SignedAmount::from_btc(pnl)
            .context("Could not convert PNL to bitcoin::SignedAmount")?;

        Ok(Self(pnl))
    }

    /// The profit and loss (PNL) from the perspective of the party going long.
    fn long(&self) -> SignedAmount {
        self.0
    }

    /// The profit and loss (PNL) from the perspective of the party going short.
    fn short(&self) -> SignedAmount {
        self.0 * -1
    }
}

/// Compute the closing price under which the party going long should get liquidated.
fn bankruptcy_price_long(
    initial_margin: Amount,
    n_contracts: u64,
    initial_price: u64,
    multiplier: Decimal,
) -> Result<u64> {
    let shift = bankruptcy_price_shift(initial_margin, multiplier, n_contracts)
        .context("Could not calculate long's bankruptcy price shift")?;

    Ok(initial_price.saturating_sub(shift))
}

/// Compute the closing price over which the party going short should get liquidated.
fn bankruptcy_price_short(
    initial_margin: Amount,
    n_contracts: u64,
    initial_price: u64,
    multiplier: Decimal,
) -> Result<u64> {
    let shift = bankruptcy_price_shift(initial_margin, multiplier, n_contracts)
        .context("Could not calculate short's bankruptcy price shift")?;

    Ok(initial_price + shift)
}

/// By how much the price of the asset needs to shift from the initial price in order to reach the
/// bankruptcy price of the party that put up `initial_margin`.
///
/// This is an absolute value. How to apply it in order to calculate the bankruptcy price will
/// depend on the party's position.
fn bankruptcy_price_shift(
    initial_margin: Amount,
    multiplier: Decimal,
    n_contracts: u64,
) -> Result<u64> {
    let initial_margin = Decimal::from_f64(initial_margin.as_btc())
        .context("Could not create Decimal from initial margin")?;

    let n_contracts = Decimal::from(n_contracts);

    let price = initial_margin / (multiplier * n_contracts);
    let price = price
        .to_u64()
        .context("Could not convert bankruptcy price to u64")?;

    Ok(price)
}

#[cfg(test)]
mod api_tests {
    use super::*;
    use crate::payout_curve::prop_compose::arb_fee_flow;
    use crate::payout_curve::prop_compose::arb_leverage;
    use crate::payout_curve::quanto;
    use itertools::Itertools;
    use proptest::prelude::*;
    use rust_decimal_macros::dec;

    #[test]
    fn quanto_curve_snapshot() {
        let initial_price = 1_000;
        let n_contracts = 100;
        let leverage_long = Leverage::TWO;
        let leverage_short = Leverage::ONE;
        let n_payouts = 20;
        let multiplier = dec!(0.000001);
        let fee_offset = CompleteFee::None;

        let payouts = Payouts::new(
            initial_price,
            n_contracts,
            leverage_long,
            leverage_short,
            n_payouts,
            multiplier,
            fee_offset,
        )
        .unwrap()
        .0;
        let expected_payouts = vec![
            payout(0..=500, 15000000, 0),
            payout(501..=584, 14580000, 420000),
            payout(585..=668, 13740000, 1260000),
            payout(669..=752, 12900000, 2100000),
            payout(753..=836, 12060000, 2940000),
            payout(837..=920, 11220000, 3780000),
            payout(921..=1004, 10380000, 4620000),
            payout(1005..=1088, 9540000, 5460000),
            payout(1089..=1172, 8700000, 6300000),
            payout(1173..=1256, 7860000, 7140000),
            payout(1257..=1340, 7020000, 7980000),
            payout(1341..=1424, 6180000, 8820000),
            payout(1425..=1508, 5340000, 9660000),
            payout(1509..=1592, 4500000, 10500000),
            payout(1593..=1676, 3660000, 11340000),
            payout(1677..=1760, 2820000, 12180000),
            payout(1761..=1844, 1980000, 13020000),
            payout(1845..=1928, 1140000, 13860000),
            payout(1929..=1999, 360000, 14640000),
            payout(2000..=1048575, 0, 15000000),
        ];

        assert_eq!(payouts, expected_payouts)
    }

    proptest! {
        #[test]
        fn payout_totals_are_equal(
            initial_price in 1u64..100_000,
            n_contracts in 1u64..10_000,
            leverage_long in arb_leverage(1, 100),
            leverage_short in arb_leverage(1, 100),
            n_payouts in 10usize..2000,
            fee_offset in arb_fee_flow(-100_000, 100_000)
        ) {
            let payouts = generate_payouts(
                initial_price,
                n_contracts,
                leverage_long,
                leverage_short,
                n_payouts,
                dec!(0.000001),
                fee_offset
            )?;

            prop_assert!(
                payouts
                    .0
                    .iter()
                    .map(|payout| payout.long + payout.short)
                    .all_equal()
            );
        }
    }

    proptest! {
        #[test]
        fn payout_intervals_have_no_gaps(
            initial_price in 1u64..100_000,
            n_contracts in 1u64..10_000,
            leverage_long in arb_leverage(1, 100),
            leverage_short in arb_leverage(1, 100),
            n_payouts in 10usize..2000,
            fee_offset in arb_fee_flow(-100_000, 100_000)
        ) {
            let payouts = generate_payouts(
                initial_price,
                n_contracts,
                leverage_long,
                leverage_short,
                n_payouts,
                dec!(0.000001),
                fee_offset
            )?;
            let payouts = payouts.0;

            let are_payout_intervals_gap_free = payouts
                .iter()
                .zip(payouts.iter().skip(1))
                .all(|(a, b)| a.interval.end() + 1 == *b.interval.start());

            prop_assert!(are_payout_intervals_gap_free)


        }
    }

    proptest! {
        #[test]
        fn payout_intervals_are_monotonically_increasing(
            initial_price in 1u64..100_000,
            n_contracts in 1u64..10_000,
            leverage_long in arb_leverage(1, 100),
            leverage_short in arb_leverage(1, 100),
            n_payouts in 10usize..2000,
            fee_offset in arb_fee_flow(-100_000, 100_000)
        ) {
            let payouts = generate_payouts(
                initial_price,
                n_contracts,
                leverage_long,
                leverage_short,
                n_payouts,
                dec!(0.000001),
                fee_offset
            )?;
            let payouts = payouts.0;


            let are_payout_intervals_monotonically_increasing = payouts
                .iter()
                .all(|a| a.interval.start() <= a.interval.end());

            prop_assert!(are_payout_intervals_monotonically_increasing)
        }
    }

    /// Helper function which generates quanto payouts in a proptest-friendly way.
    ///
    /// The `Payouts::new` API correctly fails if the `fee_offset` provided is greater than the
    /// initial margin of the party who has to pay for it. Because we sample arbitrary data for the
    /// property-based tests, we sometimes generate data that will run into
    /// `quanto::Error::{Long,Short}OwesTooMuch`. We don't want to treat those as failures, so we
    /// _reject_ them instead.
    fn generate_payouts(
        initial_price: u64,
        n_contracts: u64,
        leverage_long: Leverage,
        leverage_short: Leverage,
        n_payouts: usize,
        multiplier: Decimal,
        fee_offset: CompleteFee,
    ) -> Result<Payouts, TestCaseError> {
        let res = Payouts::new(
            initial_price,
            n_contracts,
            leverage_long,
            leverage_short,
            n_payouts,
            multiplier,
            fee_offset,
        );

        match res {
            Ok(payouts) => Ok(payouts),
            Err(quanto::Error::LongOwesTooMuch { .. })
            | Err(quanto::Error::ShortOwesTooMuch { .. }) => Err(TestCaseError::reject(
                "The fee_offset was too high, given the other parameters",
            )),
            Err(e) => Err(TestCaseError::fail(format!("{e}"))),
        }
    }

    /// Helper function to construct payouts in a readable way for the tests.
    fn payout(interval: RangeInclusive<u64>, short: u64, long: u64) -> Payout {
        Payout {
            interval,
            long: Amount::from_sat(long),
            short: Amount::from_sat(short),
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn initial_margin_snapshot() {
        let curve = Curve::new(
            1_000,
            100,
            Leverage::new(10).unwrap(),
            Leverage::ONE,
            200,
            dec!(0.000001),
            CompleteFee::None,
        );

        let initial_margin_long = curve.initial_margin_long();
        let initial_margin_short = curve.initial_margin_short();

        assert_eq!(initial_margin_long, Amount::from_sat(1_000_000));
        assert_eq!(initial_margin_short, Amount::from_sat(10_000_000));
    }

    #[test]
    fn pnl_at_closing_price_snapshot() {
        let opening_price = 1_000;
        let curve = Curve::new(
            opening_price,
            100,
            Leverage::new(10).unwrap(),
            Leverage::ONE,
            200,
            dec!(0.000001),
            CompleteFee::None,
        );

        let pnl = curve.pnl_at_closing_price(opening_price).unwrap().0;
        assert_eq!(pnl, SignedAmount::ZERO);

        {
            let long_pnl = curve.pnl_at_closing_price(2_000).unwrap().long();
            let short_pnl = curve.pnl_at_closing_price(2_000).unwrap().short();

            assert_eq!(long_pnl, SignedAmount::from_sat(10000000));
            assert_eq!(short_pnl, SignedAmount::from_sat(-10000000));
        }

        {
            let long_pnl = curve.pnl_at_closing_price(500).unwrap().long();
            let short_pnl = curve.pnl_at_closing_price(500).unwrap().short();

            assert_eq!(long_pnl, SignedAmount::from_sat(-5000000));
            assert_eq!(short_pnl, SignedAmount::from_sat(5000000));
        }
    }

    #[test]
    fn liquidation_intervals_snapshot() {
        let curve = Curve::new(
            10_000,
            500,
            Leverage::ONE,
            Leverage::new(4).unwrap(),
            200,
            dec!(0.000001),
            CompleteFee::None,
        );

        let long_liquidation_interval = curve.long_liquidation_interval().unwrap();
        let short_liquidation_interval = curve.short_liquidation_interval().unwrap();

        assert_eq!(long_liquidation_interval, 0..=0);
        assert_eq!(short_liquidation_interval, 12500..=1048575);
    }
}
