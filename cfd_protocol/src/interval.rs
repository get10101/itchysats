use bit_vec::BitVec;
use std::fmt::Display;
use std::num::NonZeroU8;
use std::ops::RangeInclusive;

mod digit_decomposition;

/// Maximum supported BTC price in whole USD.
pub const MAX_PRICE_DEC: u64 = (BASE as u64).pow(MAX_DIGITS as u32) - 1;

/// Maximum number of binary digits for BTC price in whole USD.
const MAX_DIGITS: usize = 20;

const BASE: usize = 2;

/// Binary representation of a price interval.
#[derive(Clone, Debug, PartialEq)]
pub struct Digits(BitVec);

impl Digits {
    pub fn new(range: RangeInclusive<u64>) -> Result<Vec<Self>, Error> {
        let (start, end) = range.into_inner();
        if start > MAX_PRICE_DEC || end > MAX_PRICE_DEC {
            return Err(Error::RangeOverMax);
        }

        if start > end {
            return Err(Error::DecreasingRange);
        }

        let digits = digit_decomposition::group_by_ignoring_digits(
            start as usize,
            end as usize,
            BASE,
            MAX_DIGITS,
        )
        .iter()
        .map(|digits| {
            let digits = digits.iter().map(|n| *n != 0).collect::<BitVec>();
            Digits(digits)
        })
        .collect();

        Ok(digits)
    }

    /// Calculate the range of prices expressed by these digits.
    ///
    /// With the resulting range one can assess wether a particular
    /// price corresponds to the described interval.
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

    /// Map each bit to its index in the set {0, 1}, starting at 1.
    pub fn to_indices(&self) -> Vec<NonZeroU8> {
        self.0
            .iter()
            .map(|bit| NonZeroU8::new(if bit { 2u8 } else { 1u8 }).expect("1 and 2 are non-zero"))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Interval would generate values over maximum price of {MAX_PRICE_DEC}.")]
    RangeOverMax,
    #[error("Invalid decreasing interval.")]
    DecreasingRange,
}

impl Display for Digits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0
            .iter()
            .try_for_each(|digit| write!(f, "{}", digit as u8))?;

        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    use anyhow::Result;
    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    struct Interval(RangeInclusive<u64>);

    impl Interval {
        fn new(range: RangeInclusive<u64>) -> Self {
            Self(range)
        }

        fn to_digits(&self) -> Result<Vec<Digits>> {
            let digits = Digits::new(self.0.clone())?;

            Ok(digits)
        }
    }

    impl PartialEq<Vec<Digits>> for Interval {
        fn eq(&self, other: &Vec<Digits>) -> bool {
            let sub_intervals = other.iter().flat_map(|i| i.range());
            sub_intervals.eq(self.0.clone())
        }
    }

    prop_compose! {
        fn interval()(x in 0u64..=MAX_PRICE_DEC, y in 0u64..=MAX_PRICE_DEC) -> Interval {
            let (start, end) = if x < y { (x, y) } else { (y, x) };

            Interval::new(start..=end)
        }
    }

    proptest! {
        #[test]
        fn interval_equal_to_sum_of_sub_intervals_described_by_digits(interval in interval()) {
            prop_assert!(interval == interval.to_digits().unwrap())
        }
    }
}
