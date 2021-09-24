fn main() -> std::io::Result<()> {
    std::fs::create_dir_all("../frontend/dist/maker")?;
    std::fs::create_dir_all("../frontend/dist/taker")?;

    Ok(())
}
