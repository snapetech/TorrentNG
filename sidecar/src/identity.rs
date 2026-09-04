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
use std::{fs::File, io::Read, path::Path};

pub const PEER_ID_PREFIX: &str = "-lt100B-";
const PEER_ID_SUFFIX_FILE: &str = "peer_id_suffix";
const MAX_PEER_ID_SUFFIX_BYTES: u64 = 4096;

/// Load this install's persisted 12-byte peer id suffix from
/// `<data_dir>/peer_id_suffix`, generating and persisting a new random one
/// if none exists yet, and return the full 20-byte peer id string.
pub fn load_or_generate_peer_id(data_dir: &Path) -> Result<String> {
    let path = data_dir.join(PEER_ID_SUFFIX_FILE);
    if let Ok(existing) = read_bounded_text(&path, MAX_PEER_ID_SUFFIX_BYTES) {
        let trimmed = existing.trim();
        if valid_suffix(trimmed) {
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

fn read_bounded_text(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "peer id suffix file exceeds size limit",
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "peer id suffix file is not valid UTF-8",
        )
    })
}

fn valid_suffix(suffix: &str) -> bool {
    suffix.len() == 12
        && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && suffix.bytes().any(|byte| byte != b'0')
}

fn random_suffix() -> String {
    use rand::distr::{Alphanumeric, SampleString};
    Alphanumeric.sample_string(&mut rand::rng(), 12)
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

    #[test]
    fn invalid_persisted_suffix_is_replaced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(PEER_ID_SUFFIX_FILE);
        std::fs::write(&path, "000000000000").expect("write sentinel");

        let id = load_or_generate_peer_id(dir.path()).expect("replace sentinel");
        let suffix = id.strip_prefix(PEER_ID_PREFIX).expect("prefix");

        assert!(valid_suffix(suffix));
        assert_ne!(suffix, "000000000000");
        assert_eq!(
            std::fs::read_to_string(path).expect("read replacement"),
            suffix
        );
    }

    #[test]
    fn punctuation_in_persisted_suffix_is_replaced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(PEER_ID_SUFFIX_FILE);
        std::fs::write(&path, "aaaaaaaaaaa!").expect("write invalid suffix");

        let id = load_or_generate_peer_id(dir.path()).expect("replace invalid suffix");
        let suffix = id.strip_prefix(PEER_ID_PREFIX).expect("prefix");

        assert!(valid_suffix(suffix));
    }

    #[test]
    fn oversized_persisted_suffix_is_replaced_without_unbounded_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(PEER_ID_SUFFIX_FILE);
        std::fs::write(&path, vec![b'a'; (MAX_PEER_ID_SUFFIX_BYTES + 1) as usize])
            .expect("write oversized suffix");

        let id = load_or_generate_peer_id(dir.path()).expect("replace oversized suffix");
        let suffix = id.strip_prefix(PEER_ID_PREFIX).expect("prefix");
        assert!(valid_suffix(suffix));
    }
}
