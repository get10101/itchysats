use anyhow::Result;

fn main() -> Result<()> {
    std::fs::create_dir_all("../maker-frontend/dist/maker")?;
    std::fs::create_dir_all("../taker-frontend/dist/taker")?;
    Ok(())
}
