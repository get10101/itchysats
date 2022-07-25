use anyhow::Result;
use vergen::vergen;
use vergen::Config;
use vergen::SemverKind;

fn main() -> Result<()> {
    let mut config = Config::default();
    *config.git_mut().semver_kind_mut() = SemverKind::Lightweight;

    vergen(config)
}
