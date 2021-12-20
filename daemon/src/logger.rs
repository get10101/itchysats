use anyhow::Result;
use console_subscriber::ConsoleLayer;
use time::macros::format_description;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::time::LocalTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

pub fn init(level: LevelFilter, json_format: bool) -> Result<()> {
    if level == LevelFilter::OFF {
        return Ok(());
    }

    let is_terminal = atty::is(atty::Stream::Stderr);

    let filter = EnvFilter::from_default_env()
        .add_directive(format!("taker={}", level).parse()?)
        .add_directive(format!("maker={}", level).parse()?)
        .add_directive(format!("daemon={}", level).parse()?)
        .add_directive(format!("rocket={}", level).parse()?)
        .add_directive(format!("tokio=trace").parse()?)
        .add_directive(format!("runtime=trace").parse()?);

    let timer = LocalTime::new(format_description!("[hour]:[minute]:[second]"));
    let builder = FmtSubscriber::builder()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(is_terminal)
        .with_timer(timer);

    if json_format {
        builder.json().init();
    } else {
        builder.init();
    }

    tracing::info!("Initialized logger");

    Ok(())
}
