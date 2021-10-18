use anyhow::Result;
use vergen::{vergen, Config, SemverKind};

fn main() -> Result<()> {
    std::fs::create_dir_all("../frontend/dist/maker")?;
    std::fs::create_dir_all("../frontend/dist/taker")?;

    let mut config = Config::default();
    *config.git_mut().semver_kind_mut() = SemverKind::Lightweight;

    vergen(config)
}
