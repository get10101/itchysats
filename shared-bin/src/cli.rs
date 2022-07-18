use std::path::PathBuf;

use clap::Parser;
use clap::Subcommand;
use daemon::bdk::bitcoin;
use daemon::bdk::bitcoin::Address;
use daemon::bdk::bitcoin::Amount;

const MAINNET_ELECTRUM: &str = "ssl://blockstream.info:700";
const TESTNET_ELECTRUM: &str = "ssl://blockstream.info:993";

#[derive(Parser, Clone)]
pub enum Network {
    /// Run on mainnet (default)
    Mainnet {
        /// URL to the electrum backend to use for the wallet.
        #[clap(long, default_value = MAINNET_ELECTRUM)]
        electrum: String,

        #[clap(subcommand)]
        withdraw: Option<Withdraw>,
    },
    /// Run on testnet
    Testnet {
        /// URL to the electrum backend to use for the wallet.
        #[clap(long, default_value = TESTNET_ELECTRUM)]
        electrum: String,

        #[clap(subcommand)]
        withdraw: Option<Withdraw>,
    },
    /// Run on signet
    Signet {
        /// URL to the electrum backend to use for the wallet.
        #[clap(long)]
        electrum: String,

        #[clap(subcommand)]
        withdraw: Option<Withdraw>,
    },
    /// Run on regtest
    Regtest {
        /// URL to the electrum backend to use for the wallet.
        #[clap(long)]
        electrum: String,

        #[clap(subcommand)]
        withdraw: Option<Withdraw>,
    },
}

impl Default for Network {
    fn default() -> Self {
        Network::Mainnet {
            electrum: MAINNET_ELECTRUM.to_string(),
            withdraw: None,
        }
    }
}

#[derive(Subcommand, Clone)]
pub enum Withdraw {
    Withdraw {
        /// Optionally specify the amount of Bitcoin to be withdrawn. If not specified the wallet
        /// will be drained. Amount is to be specified with denomination, e.g. "0.1 BTC"
        #[clap(long)]
        amount: Option<Amount>,
        /// Optionally specify the fee-rate for the transaction. The fee-rate is specified as sats
        /// per vbyte, e.g. 5.0
        #[clap(long)]
        fee: Option<f32>,
        /// The address to receive the Bitcoin.
        #[clap(long)]
        address: Address,
    },
}

impl Network {
    pub fn electrum(&self) -> &str {
        match self {
            Network::Mainnet { electrum, .. } => electrum,
            Network::Testnet { electrum, .. } => electrum,
            Network::Signet { electrum, .. } => electrum,
            Network::Regtest { electrum, .. } => electrum,
        }
    }

    pub fn bitcoin_network(&self) -> bitcoin::Network {
        match self {
            Network::Mainnet { .. } => bitcoin::Network::Bitcoin,
            Network::Testnet { .. } => bitcoin::Network::Testnet,
            Network::Signet { .. } => bitcoin::Network::Signet,
            Network::Regtest { .. } => bitcoin::Network::Regtest,
        }
    }

    pub fn bitmex_network(&self) -> bitmex_stream::Network {
        match self {
            Network::Mainnet { .. } => bitmex_stream::Network::Mainnet,
            Network::Testnet { .. } | Network::Signet { .. } | Network::Regtest { .. } => {
                bitmex_stream::Network::Testnet
            }
        }
    }

    pub fn data_dir(&self, base: PathBuf) -> PathBuf {
        match self {
            Network::Mainnet { .. } => base.join("mainnet"),
            Network::Testnet { .. } => base.join("testnet"),
            Network::Signet { .. } => base.join("signet"),
            Network::Regtest { .. } => base.join("regtest"),
        }
    }

    pub fn withdraw(&self) -> &Option<Withdraw> {
        match self {
            Network::Mainnet { withdraw, .. } => withdraw,
            Network::Testnet { withdraw, .. } => withdraw,
            Network::Signet { withdraw, .. } => withdraw,
            Network::Regtest { withdraw, .. } => withdraw,
        }
    }

    /// Stringified network kind
    pub fn kind(&self) -> &str {
        match self {
            Network::Mainnet { .. } => "mainnet",
            Network::Testnet { .. } => "testnet",
            Network::Signet { .. } => "signet",
            Network::Regtest { .. } => "regtest",
        }
    }
}

impl std::fmt::Debug for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Network::Mainnet { .. } => "mainnet".fmt(f),
            Network::Testnet { .. } => "testnet".fmt(f),
            Network::Signet { .. } => "signet".fmt(f),
            Network::Regtest { .. } => "regtest".fmt(f),
        }
    }
}
