use anyhow::Result;

fn main() -> Result<()> {
    std::fs::create_dir_all("../../taker-frontend/dist/taker")?;
    Ok(())
}
