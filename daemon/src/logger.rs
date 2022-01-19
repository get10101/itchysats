use anyhow::anyhow;
use anyhow::Result;
use time::macros::format_description;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::time::UtcTime;
use tracing_subscriber::EnvFilter;

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

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(is_terminal)
        .with_timer(UtcTime::new(format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second]"
        )));

    let result = if json_format {
        builder.json().try_init()
    } else {
        builder.compact().try_init()
    };

    result.map_err(|e| anyhow!("Failed to init logger: {e}"))?;

    tracing::info!("Initialized logger");

    Ok(())
}
