use anyhow::Result;

fn main() -> Result<()> {
    std::fs::create_dir_all("../maker-frontend/dist/taker")?;
    Ok(())
}
