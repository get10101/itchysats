use anyhow::Result;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;
use xtra_libp2p::libp2p::PeerId;

/// Convenience type to load the blocked peer list from toml
#[derive(Deserialize)]
struct BlockedPeers {
    blocked: HashSet<PeerId>,
}

pub async fn load_blocked_peers(blocked_peers_path: &Path) -> Result<HashSet<PeerId>> {
    anyhow::ensure!(
        blocked_peers_path.try_exists()?,
        "No blocked peers file found in {blocked_peers_path:?}",
    );
    let raw = tokio::fs::read_to_string(blocked_peers_path).await?;
    Ok(toml::from_str::<BlockedPeers>(&raw)?.blocked)
}
