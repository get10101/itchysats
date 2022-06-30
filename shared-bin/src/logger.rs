use anyhow::anyhow;
use anyhow::Result;
use opentelemetry::global;
use opentelemetry::sdk::propagation::TraceContextPropagator;
use time::macros::format_description;
use tracing_subscriber::fmt::time::UtcTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

pub use tracing_subscriber::filter::LevelFilter;

const RUST_LOG_ENV: &str = "RUST_LOG";

#[allow(clippy::print_stdout)] // because the logger is only initialized at the end of this function but we want to print a warning
pub fn init(level: LevelFilter, json_format: bool, label: &str) -> Result<()> {
    if level == LevelFilter::OFF {
        return Ok(());
    }

    let is_terminal = atty::is(atty::Stream::Stderr);

    let filter = match std::env::var_os(RUST_LOG_ENV).map(|s| s.into_string()) {
        Some(Ok(env)) => {
            let mut filter = base_directives(EnvFilter::new(""))?;
            for directive in env.split(',') {
                match directive.parse() {
                    Ok(d) => filter = filter.add_directive(d),
                    Err(e) => println!("WARN ignoring log directive: `{directive}`: {e}"),
                };
            }
            filter
        }
        _ => base_directives(EnvFilter::from_env(RUST_LOG_ENV))?,
    };
    let filter = filter.add_directive(format!("{level}").parse()?);
    let filter_tracing: EnvFilter = filter.to_string().parse()?;

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(is_terminal);

    // let result = if json_format {
    //     builder.json().with_timer(UtcTime::rfc_3339()).try_init()
    // } else {
    //     builder
    //         .compact()
    //         .with_timer(UtcTime::new(format_description!(
    //             "[year]-[month]-[day] [hour]:[minute]:[second]"
    //         )))
    //         .try_init()
    // };

    // result.map_err(|e| anyhow!("Failed to init logger: {e}"))?;

    tracing::info!("Initialized logger");

    global::set_text_map_propagator(TraceContextPropagator::new());
    let tracer = opentelemetry_jaeger::new_pipeline()
        .with_service_name(label)
        .install_simple()
        .unwrap();

    let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(filter_tracing);

    Registry::default().with(telemetry).with(fmt_layer).init();

    Ok(())
}

fn base_directives(env: EnvFilter) -> Result<EnvFilter> {
    let filter = env
        .add_directive("bdk=warn".parse()?) // bdk is quite spamy on debug
        .add_directive("sqlx=warn".parse()?) // sqlx logs all queries on INFO
        .add_directive("hyper=warn".parse()?)
        .add_directive("rustls=warn".parse()?)
        .add_directive("reqwest=warn".parse()?)
        .add_directive("tungstenite=warn".parse()?)
        .add_directive("tokio_tungstenite=warn".parse()?)
        .add_directive("electrum_client=warn".parse()?)
        .add_directive("want=warn".parse()?)
        .add_directive("mio=warn".parse()?)
        .add_directive("tokio_util=warn".parse()?)
        .add_directive("yamux=warn".parse()?)
        .add_directive("multistream_select=warn".parse()?)
        .add_directive("libp2p_noise=warn".parse()?)
        .add_directive("xtra_libp2p_offer=debug".parse()?)
        .add_directive("xtras=info".parse()?)
        .add_directive("_=off".parse()?) // rocket logs headers on INFO and uses `_` as the log target for it?
        .add_directive("rocket=off".parse()?) // disable rocket logs: we have our own
        .add_directive("xtra=warn".parse()?)
        .add_directive("sled=warn".parse()?) // downgrade sled log level: it is spamming too much on DEBUG
        .add_directive("xtra_libp2p=info".parse()?);
    Ok(filter)
}
