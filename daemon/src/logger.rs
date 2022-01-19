use anyhow::Result;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::time::ChronoLocal;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::FmtSubscriber;

pub fn init(level: LevelFilter, json_format: bool) -> Result<()> {
    if level == LevelFilter::OFF {
        return Ok(());
    }

    let is_terminal = atty::is(atty::Stream::Stderr);

    let filter = EnvFilter::from_default_env()
        .add_directive(format!("taker={level}").parse()?)
        .add_directive(format!("maker={level}").parse()?)
        .add_directive(format!("daemon={level}").parse()?)
        .add_directive(format!("rocket={level}").parse()?);

    let builder = FmtSubscriber::builder()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(is_terminal)
        .with_timer(ChronoLocal::with_format("%F %T".to_owned()));

    if json_format {
        builder.json().init();
    } else {
        builder.init();
    }

    tracing::info!("Initialized logger");

    Ok(())
}
