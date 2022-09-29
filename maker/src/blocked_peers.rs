use anyhow::Result;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;
use xtra_libp2p::libp2p::PeerId;

const FILENAME: &str = "blocked_peers.toml";

/// Convenience type to load the blocked peer list from toml
#[derive(Deserialize)]
struct BlockedPeers {
    blocked: HashSet<PeerId>,
}

pub async fn load_blocked_peers(directory: &Path) -> Result<HashSet<PeerId>> {
    let path = directory.join(FILENAME);

    if !path.try_exists()? {
        tracing::info!("No blocked peers. Expected config file at: {path:?}");

        return Ok(HashSet::default());
    }

    let raw = tokio::fs::read_to_string(path).await?;
    Ok(toml::from_str::<BlockedPeers>(&raw)?.blocked)
}
