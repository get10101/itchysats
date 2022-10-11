pub mod catchers;
pub mod cli;
pub mod fairings;
pub mod logger;
pub mod routes;
mod to_sse_event;

pub use crate::to_sse_event::*;

pub const MAINNET_ELECTRUM: &str = "ssl://blockstream.info:700";
pub const TESTNET_ELECTRUM: &str = "ssl://blockstream.info:993";
