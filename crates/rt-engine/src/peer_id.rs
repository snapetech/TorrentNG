use std::path::Path;
use std::sync::OnceLock;

/// Upstream rTorrent/libtorrent 0.16.11 peer ID family prefix (8 bytes).
///
/// libtorrent 0.16.11 uses PEER_NAME "-lt100B-". Keep this fixed: some
/// private trackers only allow known client families through, matched on
/// this prefix.
pub const PEER_ID_PREFIX: &[u8; 8] = b"-lt100B-";

/// Historical fully-pinned peer ID. Every install that has not resolved a
/// real per-install identity yet compares against this literal.
///
/// Do NOT use this as a live identity: it has an all-zero suffix, so every
/// TorrentNG install that skips identity resolution presents the exact same
/// 20 bytes. A tracker (MAM's client/peer verification included) reads the
/// same peer_id showing up from many different IPs as the same client
/// running multiple simultaneous instances — a bannable multi-client
/// signature — even though each install is actually a different, unrelated
/// user. See docs/TRACKER-IDENTITY.md.
pub const DEFAULT_PEER_ID: [u8; 20] = *b"-lt100B-000000000000";

/// HTTP `User-Agent` header sent on tracker announces and scrapes.
///
/// Do not strip this to "rtorrent/0.16.11"; some trackers validate the full
/// upstream rTorrent/libtorrent pair. Unlike peer_id, sharing this string
/// across installs is fine: the User-Agent header is not expected to be a
/// per-instance-unique identifier.
pub const DEFAULT_USER_AGENT: &str = "rtorrent/0.16.11/0.16.11";

const PEER_ID_SUFFIX_FILE: &str = "peer_id_suffix";

static PEER_ID: OnceLock<[u8; 20]> = OnceLock::new();
static USER_AGENT: OnceLock<String> = OnceLock::new();

/// Resolve and persist the per-install peer ID before any tracker/peer code
/// can observe [`our_peer_id`]. Call this exactly once, early in daemon
/// startup, with the daemon's session directory.
///
/// Priority order: `TORRENTNG_PEER_ID`/`TNG_PEER_ID` env override, then a
/// 12-byte suffix persisted at `<session_dir>/peer_id_suffix` from a prior
/// run, then a freshly generated random suffix written to that file so
/// future restarts stay stable. If the caller never invokes this (e.g. a
/// standalone test), [`our_peer_id`] still self-initializes, but with an
/// unpersisted random suffix rather than the shared literal default.
pub fn init(session_dir: &Path) {
    let _ = PEER_ID.get_or_init(|| resolve_peer_id(Some(session_dir)));
}

pub fn our_peer_id() -> [u8; 20] {
    *PEER_ID.get_or_init(|| resolve_peer_id(None))
}

fn resolve_peer_id(session_dir: Option<&Path>) -> [u8; 20] {
    if let Some(id) = env_peer_id() {
        return id;
    }
    let suffix = session_dir
        .and_then(|dir| match load_or_generate_suffix(dir) {
            Ok(suffix) => Some(suffix),
            Err(error) => {
                tracing::warn!(
                    component = "peer_id",
                    operation = "resolve",
                    result = "error",
                    %error,
                    "could not persist per-install peer id suffix; using an unpersisted random one for this run"
                );
                None
            }
        })
        .unwrap_or_else(random_suffix);
    build_peer_id(&suffix)
}

fn env_peer_id() -> Option<[u8; 20]> {
    std::env::var("TORRENTNG_PEER_ID")
        .or_else(|_| std::env::var("TNG_PEER_ID"))
        .ok()
        .and_then(|value| {
            let bytes = value.as_bytes();
            if bytes.len() != 20 || !bytes.is_ascii() {
                return None;
            }
            let mut peer_id = [0u8; 20];
            peer_id.copy_from_slice(bytes);
            Some(peer_id)
        })
}

fn build_peer_id(suffix: &str) -> [u8; 20] {
    let mut id = [0u8; 20];
    id[..8].copy_from_slice(PEER_ID_PREFIX);
    id[8..].copy_from_slice(suffix.as_bytes());
    id
}

/// Load a previously-generated 12-byte suffix from `<session_dir>/peer_id_suffix`,
/// or generate and persist a new one if none exists yet (fresh install or
/// upgrade from a version that had no persisted identity).
fn load_or_generate_suffix(session_dir: &Path) -> std::io::Result<String> {
    let path = session_dir.join(PEER_ID_SUFFIX_FILE);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if trimmed.len() == 12 && trimmed.is_ascii() {
            return Ok(trimmed.to_owned());
        }
    }
    let suffix = random_suffix();
    std::fs::create_dir_all(session_dir)?;
    std::fs::write(&path, &suffix)?;
    Ok(suffix)
}

fn random_suffix() -> String {
    use rand::distributions::{Alphanumeric, DistString};
    Alphanumeric.sample_string(&mut rand::thread_rng(), 12)
}

pub fn user_agent() -> &'static str {
    USER_AGENT
        .get_or_init(|| {
            std::env::var("TORRENTNG_USER_AGENT")
                .or_else(|_| std::env::var("TNG_USER_AGENT"))
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_USER_AGENT.to_owned())
        })
        .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_identity_is_upstream_rtorrent_0_16_11_pair() {
        assert_eq!(DEFAULT_PEER_ID.len(), 20);
        assert!(DEFAULT_PEER_ID.starts_with(PEER_ID_PREFIX));
        assert_eq!(DEFAULT_USER_AGENT, "rtorrent/0.16.11/0.16.11");
    }

    #[test]
    fn independently_generated_peer_ids_do_not_collide() {
        // This is the exact property the old hardcoded "-lt100B-000000000000"
        // default broke: every install generated the identical 20 bytes.
        let a = build_peer_id(&random_suffix());
        let b = build_peer_id(&random_suffix());
        assert_ne!(a, b, "two independently generated peer ids must differ");
        assert!(a.starts_with(PEER_ID_PREFIX));
        assert_eq!(a.len(), 20);
    }

    #[test]
    fn persisted_suffix_is_stable_across_reloads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = load_or_generate_suffix(dir.path()).expect("first load");
        let second = load_or_generate_suffix(dir.path()).expect("second load");
        assert_eq!(first, second, "restarting must not change the peer id");
    }

    #[test]
    fn init_persists_and_is_idempotent_for_this_process() {
        // OnceLock means a real second `init` call in the same process can't
        // be exercised here without a fresh static; this instead checks that
        // resolving twice against the same directory (simulating two
        // process restarts) yields the same peer id both times.
        let dir = tempfile::tempdir().expect("tempdir");
        let first = resolve_peer_id(Some(dir.path()));
        let second = resolve_peer_id(Some(dir.path()));
        assert_eq!(first, second);
    }
}
