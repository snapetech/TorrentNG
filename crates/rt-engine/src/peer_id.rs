use std::sync::OnceLock;

/// Upstream rTorrent/libtorrent 0.16.11 tracker identity pair.
///
/// Keep this paired with DEFAULT_USER_AGENT. libtorrent 0.16.11 uses
/// PEER_NAME "-lt100B-" and rTorrent 0.16.11 builds its tracker User-Agent as
/// "rtorrent/0.16.11/0.16.11".
pub const DEFAULT_PEER_ID: [u8; 20] = *b"-lt100B-000000000000";

/// HTTP `User-Agent` header sent on tracker announces and scrapes.
///
/// Do not strip this to "rtorrent/0.16.11"; some trackers validate the full
/// upstream rTorrent/libtorrent pair.
pub const DEFAULT_USER_AGENT: &str = "rtorrent/0.16.11/0.16.11";

static PEER_ID: OnceLock<[u8; 20]> = OnceLock::new();
static USER_AGENT: OnceLock<String> = OnceLock::new();

pub fn our_peer_id() -> [u8; 20] {
    *PEER_ID.get_or_init(|| {
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
            .unwrap_or(DEFAULT_PEER_ID)
    })
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
    use super::{DEFAULT_PEER_ID, DEFAULT_USER_AGENT};

    #[test]
    fn default_identity_is_upstream_rtorrent_0_16_11_pair() {
        assert_eq!(DEFAULT_PEER_ID.len(), 20);
        assert!(DEFAULT_PEER_ID.starts_with(b"-lt100B-"));
        assert_eq!(DEFAULT_USER_AGENT, "rtorrent/0.16.11/0.16.11");
    }
}
