//! Per-install tracker peer identity.
//!
//! See docs/TRACKER-IDENTITY.md. The 8-byte client-family prefix
//! (`-lt100B-`) is intentionally shared across every TorrentNG install so
//! trackers that whitelist known client families accept it. The remaining
//! 12 bytes are NOT supposed to be shared: they exist specifically so a
//! tracker can tell separate client instances apart. A hardcoded suffix
//! (the historical `000000000000`) makes every unconfigured install present
//! byte-identical peer ids, which reads to tracker anti-cheat as the same
//! client running many simultaneous instances.

use anyhow::{Context, Result};
use std::path::Path;

pub const PEER_ID_PREFIX: &str = "-lt100B-";
const PEER_ID_SUFFIX_FILE: &str = "peer_id_suffix";

/// Load this install's persisted 12-byte peer id suffix from
/// `<data_dir>/peer_id_suffix`, generating and persisting a new random one
/// if none exists yet, and return the full 20-byte peer id string.
pub fn load_or_generate_peer_id(data_dir: &Path) -> Result<String> {
    let path = data_dir.join(PEER_ID_SUFFIX_FILE);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if trimmed.len() == 12 && trimmed.is_ascii() {
            return Ok(format!("{PEER_ID_PREFIX}{trimmed}"));
        }
    }
    let suffix = random_suffix();
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("create data dir {}", data_dir.display()))?;
    std::fs::write(&path, &suffix)
        .with_context(|| format!("persist peer id suffix to {}", path.display()))?;
    Ok(format!("{PEER_ID_PREFIX}{suffix}"))
}

fn random_suffix() -> String {
    use rand::distributions::{Alphanumeric, DistString};
    Alphanumeric.sample_string(&mut rand::thread_rng(), 12)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_peer_id_has_correct_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let id = load_or_generate_peer_id(dir.path()).expect("generate");
        assert_eq!(id.len(), 20);
        assert!(id.is_ascii());
        assert!(id.starts_with(PEER_ID_PREFIX));
    }

    #[test]
    fn persisted_peer_id_is_stable_across_reloads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = load_or_generate_peer_id(dir.path()).expect("first");
        let second = load_or_generate_peer_id(dir.path()).expect("second");
        assert_eq!(first, second);
    }

    #[test]
    fn independent_installs_do_not_collide() {
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let a = load_or_generate_peer_id(dir_a.path()).expect("a");
        let b = load_or_generate_peer_id(dir_b.path()).expect("b");
        assert_ne!(a, b, "two independent installs must not share a peer id");
    }
}
