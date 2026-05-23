use std::sync::OnceLock;

/// rTorrent/libtorrent 0.16.11-compatible peer ID prefix plus 12 stable bytes.
/// Some private trackers validate both peer ID family and HTTP User-Agent.
pub const DEFAULT_PEER_ID: [u8; 20] = *b"-lt100B-000000000000";

/// HTTP `User-Agent` header sent on tracker announces and scrapes.
pub const DEFAULT_USER_AGENT: &str = "rtorrent/0.16.11";

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
