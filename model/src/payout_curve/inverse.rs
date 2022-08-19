use crate::Contracts;
use crate::Leverage;
use crate::Price;
use bdk::bitcoin::Amount;

mod implementation;

pub use implementation::calculate;

/// Calculates the margin in BTC
///
/// The initial margin represents the collateral both parties have to come up with
/// to satisfy the contract.
pub fn calculate_margin(price: Price, quantity: Contracts, leverage: Leverage) -> Amount {
    quantity / (price * leverage)
}
