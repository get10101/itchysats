use anyhow::Result;
use taker::run;
use taker::Opts;

#[rocket::main]
async fn main() -> Result<()> {
    let opts = Opts::read();
    run(opts).await
}
