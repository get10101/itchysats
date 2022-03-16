pub fn version() -> &'static str {
    env!("VERGEN_GIT_SEMVER_LIGHTWEIGHT")
}
