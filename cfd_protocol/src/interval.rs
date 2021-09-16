use anyhow::{bail, Result};
use bit_vec::BitVec;
use std::fmt::Display;
use std::ops::RangeInclusive;

mod digit_decomposition;

const BASE: usize = 2;

/// Maximum number of binary digits for BTC price in whole USD.
const MAX_DIGITS: usize = 20;

const MAX_PRICE_DEC: u64 = (BASE as u64).pow(MAX_DIGITS as u32);

#[derive(Debug)]
pub struct Interval(RangeInclusive<u64>);

impl Interval {
    pub fn new(start: u64, end: u64) -> Result<Self> {
        if start > MAX_PRICE_DEC || end > MAX_PRICE_DEC {
            bail!("price over maximum")
        }

        if start > end {
            bail!("invalid interval: start > end")
        }

        Ok(Self(start..=end))
    }

    pub fn as_digits(&self) -> Vec<Digits> {
        digit_decomposition::group_by_ignoring_digits(
            *self.0.start() as usize,
            *self.0.end() as usize,
            BASE,
            MAX_DIGITS,
        )
        .iter()
        .map(|digits| {
            let digits = digits.iter().map(|n| *n != 0).collect::<BitVec>();
            Digits(digits)
        })
        .collect()
    }
}

#[derive(Clone, Debug)]
pub struct Digits(BitVec);

impl Digits {
    pub fn range(&self) -> RangeInclusive<u64> {
        let missing_bits = MAX_DIGITS - self.0.len();

        let mut bits = self.0.clone();
        bits.append(&mut BitVec::from_elem(missing_bits, false));
        let start = bits.as_u64();

        let mut bits = self.0.clone();
        bits.append(&mut BitVec::from_elem(missing_bits, true));
        let end = bits.as_u64();

        start..=end
    }
}

impl std::iter::Iterator for Digits {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.iter().map(|bit| vec![bit as u8]).next()
    }
}

trait BitVecExt {
    fn as_u64(&self) -> u64;
}

impl BitVecExt for BitVec {
    fn as_u64(&self) -> u64 {
        let len = self.len();

        self.iter().enumerate().fold(0, |acc, (i, x)| {
            acc + ((x as u64) * (BASE.pow((len - i - 1) as u32) as u64))
        })
    }
}

impl Display for Digits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0
            .iter()
            .try_for_each(|digit| write!(f, "{}", digit as u8))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl PartialEq<Vec<Digits>> for Interval {
        fn eq(&self, other: &Vec<Digits>) -> bool {
            let sub_intervals = other.iter().flat_map(|i| i.range());
            sub_intervals.eq(self.0.clone())
        }
    }

    prop_compose! {
        fn interval()(x in 0u64..=MAX_PRICE_DEC, y in 0u64..=MAX_PRICE_DEC) -> Interval {
            let (start, end) = if x < y { (x, y) } else { (y, x) };

            Interval::new(start, end).unwrap()
        }
    }

    proptest! {
        #[test]
        fn interval_equal_to_sum_of_sub_intervals_described_by_digits(interval in interval()) {
            prop_assert!(interval == interval.as_digits())
        }
    }
}
