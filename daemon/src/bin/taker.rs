use anyhow::Result;
use daemon::routes_taker;

#[rocket::main]
async fn main() -> Result<()> {
    routes_taker::start_http().await?;

    Ok(())
}
