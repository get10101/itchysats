use anyhow::anyhow;
use anyhow::Result;
use time::macros::format_description;
use tracing_subscriber::fmt::time::UtcTime;
use tracing_subscriber::EnvFilter;

pub use tracing_subscriber::filter::LevelFilter;

pub fn init(level: LevelFilter, json_format: bool) -> Result<()> {
    if level == LevelFilter::OFF {
        return Ok(());
    }

    let is_terminal = atty::is(atty::Stream::Stderr);

    let filter = EnvFilter::from_default_env()
        .add_directive(format!("{level}").parse()?)
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

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(is_terminal);

    let result = if json_format {
        builder.json().with_timer(UtcTime::rfc_3339()).try_init()
    } else {
        builder
            .compact()
            .with_timer(UtcTime::new(format_description!(
                "[year]-[month]-[day] [hour]:[minute]:[second]"
            )))
            .try_init()
    };

    result.map_err(|e| anyhow!("Failed to init logger: {e}"))?;

    tracing::info!("Initialized logger");

    Ok(())
}
