/// Return git semver version
///
/// Examples:
/// * 0.5.0 - released version 0.5.0
/// * 0.5.0-119-g1d1074ce - master version, with tag 'g1d1074ce', 119 commits after 0.5.0
pub fn git_semver() -> &'static str {
    // --tags = gives the same behaviour as vergen
    // --dirty = appends '-modified' to local runs, that are not checked in the repo
    // --always = was enabled by default
    git_version::git_version!(args = ["--tags", "--always", "--dirty=-modified"])
}
