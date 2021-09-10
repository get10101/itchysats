use anyhow::Result;
use daemon::routes_maker;

#[rocket::main]
async fn main() -> Result<()> {
    routes_maker::start_http().await?;

    Ok(())
}
