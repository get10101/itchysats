#![cfg_attr(not(test), warn(clippy::unwrap_used))]

pub use bdk;
pub use maia;
pub use sqlx; // TODO: Should we expose a `Database` type to avoid leaking sqlx?

use std::time::Duration;

pub mod sqlx_ext; // Must come first because it is a macro.

pub mod bdk_ext;
pub mod cfd_actors;
pub mod command;
pub mod db;
pub mod fan_out;
mod future_ext;
pub mod keypair;
pub mod model;
pub mod monitor;
pub mod noise;
pub mod olivia;
pub mod oracle;
pub mod payout_curve;
pub mod process_manager;
pub mod projection;
pub mod routes;
pub mod seed;
pub mod setup_contract;
mod transaction_ext;
pub mod try_continue;
pub mod wallet;
pub mod wire;

/// Duration between the heartbeats sent by the maker, used by the taker to
/// determine whether the maker is online.
pub const HEARTBEAT_INTERVAL: std::time::Duration = Duration::from_secs(5);

pub const N_PAYOUTS: usize = 200;

/// The interval until the cfd gets settled, i.e. the attestation happens
///
/// This variable defines at what point in time the oracle event id will be chose to settle the cfd.
/// Hence, this constant defines how long a cfd is open (until it gets either settled or rolled
/// over).
///
/// Multiple code parts align on this constant:
/// - How the oracle event id is chosen when creating an order (maker)
/// - The sliding window of cached oracle announcements (maker, taker)
/// - The auto-rollover time-window (taker)
pub const SETTLEMENT_INTERVAL: time::Duration = time::Duration::hours(24);
