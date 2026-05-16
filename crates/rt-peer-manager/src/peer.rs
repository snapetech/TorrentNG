use std::net::SocketAddr;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Unique handle for a managed peer connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerId(pub Uuid);

impl PeerId {
    pub fn new() -> Self {
        PeerId(Uuid::new_v4())
    }
}

impl Default for PeerId {
    fn default() -> Self {
        Self::new()
    }
}

/// Direction of the TCP connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Inbound,
    Outbound,
}

/// Upload/download byte counters for a single peer session.
#[derive(Debug, Default, Clone)]
pub struct PeerStats {
    pub uploaded: u64,
    pub downloaded: u64,
    /// Rolling upload rate in bytes/sec (exponential moving average).
    pub upload_rate: f64,
}

impl PeerStats {
    pub fn record_upload(&mut self, bytes: u64, elapsed: Duration) {
        self.uploaded += bytes;
        let rate = bytes as f64 / elapsed.as_secs_f64().max(0.001);
        // EMA with α=0.3
        self.upload_rate = 0.3 * rate + 0.7 * self.upload_rate;
    }
}

/// Choke state from our side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChokeState {
    Choked,
    Unchoked,
}

/// All state tracked for a single managed peer connection.
#[derive(Debug)]
pub struct PeerEntry {
    pub id: PeerId,
    pub addr: SocketAddr,
    pub direction: Direction,
    pub choke_state: ChokeState,
    /// Whether the remote peer is interested in downloading from us.
    pub peer_interested: bool,
    pub stats: PeerStats,
    pub connected_at: Instant,
}

impl PeerEntry {
    pub fn new(addr: SocketAddr, direction: Direction) -> Self {
        PeerEntry {
            id: PeerId::new(),
            addr,
            direction,
            choke_state: ChokeState::Choked,
            peer_interested: false,
            stats: PeerStats::default(),
            connected_at: Instant::now(),
        }
    }

    pub fn is_unchoked(&self) -> bool {
        self.choke_state == ChokeState::Unchoked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_peer_starts_choked() {
        let peer = PeerEntry::new("127.0.0.1:6881".parse().unwrap(), Direction::Outbound);
        assert_eq!(peer.choke_state, ChokeState::Choked);
        assert!(!peer.peer_interested);
        assert!(!peer.is_unchoked());
    }

    #[test]
    fn peer_id_unique() {
        let a = PeerId::new();
        let b = PeerId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn upload_rate_ema() {
        let mut stats = PeerStats::default();
        stats.record_upload(16384, Duration::from_secs(1));
        // After one sample: 0.3 * 16384 + 0.7 * 0 = 4915.2
        assert!((stats.upload_rate - 4915.2).abs() < 1.0);
        stats.record_upload(16384, Duration::from_secs(1));
        // After two samples: 0.3 * 16384 + 0.7 * 4915.2 ≈ 8330.1
        assert!(stats.upload_rate > 4915.0 && stats.upload_rate < 20000.0);
    }
}
