use clap::Parser;
use csv::WriterBuilder;
use daemon::projection;
use daemon::{bdk::bitcoin::Network, projection::Cfd};
use futures::StreamExt;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use time::{self, Date};

#[derive(Parser)]
pub struct Opts {
    /// Path to an ItchySats database.
    #[clap(long)]
    pub db_path: PathBuf,
    /// Where to store extracted data.
    #[clap(long)]
    pub output_file: PathBuf,
}

#[derive(Debug, Serialize)]
struct Record {
    timestamp: String,
    total_margin_sats: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Extracts the margin per day from ItchySats database to a specified file");
    let opts = Opts::parse();

    let db = sqlite_db::connect(opts.db_path, false).await?;

    let mut stream = db.load_all_closed_cfds::<projection::Cfd>(Network::Bitcoin);

    let mut cfds: HashMap<Date, Vec<Cfd>> = HashMap::new();
    while let Some(cfd) = stream
        .next()
        .await
        .filter(|cfd| cfd.is_ok())
        .map(|c| c.unwrap())
    {
        // FIXME: Last update does *not* work
        // let date =
        //     OffsetDateTime::from_unix_timestamp(cfd.aggregated().last_update().seconds())?.date();
        let date = cfd.expiry_timestamp.unwrap().date();

        let today_cfds = if let Some(today) = cfds.get(&date) {
            let mut today = today.clone();
            today.push(cfd);
            today
        } else {
            vec![cfd]
        };
        cfds.insert(date, today_cfds);
    }

    let margins_by_date = cfds
        .iter()
        .map(|(date, cfds)| (*date, cfds.iter().map(|cfd| cfd.margin.as_sat()).sum()))
        .collect::<Vec<(Date, u64)>>();

    let mut wtr = WriterBuilder::new().from_path(opts.output_file)?;

    let time_format = time::format_description::parse("[year]-[month]-[day]")?;

    for (date, total_margin_sats) in margins_by_date {
        let timestamp = date.format(&time_format)?.to_string();
        wtr.serialize(Record {
            timestamp,
            total_margin_sats,
        })?;
    }
    wtr.flush()?;

    Ok(())
}
