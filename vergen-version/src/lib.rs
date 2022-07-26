/// Return git semver version
///
/// Examples:
/// * 0.5.0 (released version)
/// * 0.5.0-42-gb1f262c (local branch, 42 commits after 0.5.0 release, with git tag)
pub fn git_semver() -> &'static str {
    env!("VERGEN_GIT_SEMVER_LIGHTWEIGHT")
}
