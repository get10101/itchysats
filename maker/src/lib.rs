use bdk::bitcoin::util::bip32::ExtendedPrivKey;
use clap::Parser;
use daemon::bdk;
use shared_bin::cli::Network;
use shared_bin::logger::LevelFilter;
use shared_bin::logger::LOCAL_COLLECTOR_ENDPOINT;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;

pub use actor_system::ActorSystem;

mod actor_system;
pub mod cfd;
mod metrics;
pub mod routes;

#[derive(Debug)]
pub struct Password(String);

impl From<[u8; 32]> for Password {
    fn from(bytes: [u8; 32]) -> Self {
        Self(hex::encode(bytes))
    }
}

impl std::str::FromStr for Password {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

impl std::fmt::Display for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Parser)]
pub struct Opts {
    /// The port to listen on for libp2p connections.
    #[clap(long, default_value = "10000")]
    pub p2p_port: u16,

    /// The IP address to listen on for the HTTP API.
    #[clap(long, default_value = "127.0.0.1:8001")]
    pub http_address: SocketAddr,

    /// Where to permanently store data, defaults to the current working directory.
    #[clap(long)]
    pub data_dir: Option<PathBuf>,

    /// If enabled logs will be in json format
    #[clap(short, long)]
    pub json: bool,

    /// If enabled, logs in json format will contain a list of all ancestor spans of log events.
    /// This **only** has an effect when `json` is also enabled.
    #[clap(long)]
    pub json_span_list: bool,

    /// If enabled, traces will be exported to the OTEL collector
    #[clap(long)]
    pub instrumentation: bool,

    /// If enabled, tokio runtime can be locally debugged with tokio_console
    #[clap(long)]
    pub tokio_console: bool,

    /// If enabled, libp2p ping and offer broadcast spans will be included in the traces exported
    /// by the application.
    #[clap(long)]
    pub verbose_spans: bool,

    /// If enabled, browser UI is not automatically launched at startup.
    #[clap(long)]
    pub headless: bool,

    /// OTEL collector endpoint address
    ///
    /// If not specified it defaults to the local collector endpoint.
    #[clap(long, default_value = LOCAL_COLLECTOR_ENDPOINT )]
    pub collector_endpoint: String,

    /// Service name for OTEL.
    ///
    /// If not specified it defaults to the binary name.
    #[clap(long, default_value = "maker")]
    pub service_name: String,

    /// If enabled the application will not fail if an error occurred during db migration.
    #[clap(short, long)]
    pub ignore_migration_errors: bool,

    /// If provided will be used for internal wallet instead of a random key. The keys will be
    /// derived according to Bip84
    #[clap(short, long)]
    pub wallet_xprv: Option<ExtendedPrivKey>,

    /// Configure the log level, e.g.: one of Error, Warn, Info, Debug, Trace
    #[clap(short, long, default_value = "Debug")]
    pub log_level: LevelFilter,

    /// Password for the web interface.
    ///
    /// If not provided, the password will be loaded from the db which by default is
    /// `weareallsatoshi`
    #[clap(long)]
    pub password: Option<Password>,

    #[clap(subcommand)]
    pub network: Network,

    /// If enabled, the log will be printed to {service_name}.log in the data dir
    #[clap(long)]
    pub log_to_file: bool,
}
