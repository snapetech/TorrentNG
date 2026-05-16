/// Peer connection pool: tracks all active connections for a torrent.
use std::collections::HashMap;
use std::net::SocketAddr;

#[cfg(test)]
use crate::peer::Direction;
use crate::peer::{PeerEntry, PeerId};

/// Maximum peer connections per torrent.
pub const DEFAULT_MAX_PEERS: usize = 50;

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("peer {0:?} not found")]
    NotFound(PeerId),

    #[error("peer pool full (max {0} connections)")]
    Full(usize),

    #[error("duplicate address: {0}")]
    DuplicateAddress(SocketAddr),
}

/// Holds all `PeerEntry` records for a single torrent.
pub struct PeerPool {
    peers: HashMap<PeerId, PeerEntry>,
    max_peers: usize,
}

impl PeerPool {
    pub fn new(max_peers: usize) -> Self {
        PeerPool {
            peers: HashMap::new(),
            max_peers,
        }
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.peers.len() >= self.max_peers
    }

    pub fn add(&mut self, entry: PeerEntry) -> Result<PeerId, PoolError> {
        if self.is_full() {
            return Err(PoolError::Full(self.max_peers));
        }
        // Reject duplicate address
        if self.peers.values().any(|p| p.addr == entry.addr) {
            return Err(PoolError::DuplicateAddress(entry.addr));
        }
        let id = entry.id;
        self.peers.insert(id, entry);
        Ok(id)
    }

    pub fn remove(&mut self, id: PeerId) -> Result<PeerEntry, PoolError> {
        self.peers.remove(&id).ok_or(PoolError::NotFound(id))
    }

    pub fn get(&self, id: PeerId) -> Option<&PeerEntry> {
        self.peers.get(&id)
    }

    pub fn get_mut(&mut self, id: PeerId) -> Option<&mut PeerEntry> {
        self.peers.get_mut(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &PeerEntry> {
        self.peers.values()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut PeerEntry> {
        self.peers.values_mut()
    }

    /// Addresses of all current peers (used to avoid duplicate outbound dials).
    pub fn known_addresses(&self) -> impl Iterator<Item = SocketAddr> + '_ {
        self.peers.values().map(|p| p.addr)
    }
}

impl Default for PeerPool {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_PEERS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_peer(addr: &str) -> PeerEntry {
        PeerEntry::new(addr.parse().unwrap(), Direction::Outbound)
    }

    #[test]
    fn add_and_remove() {
        let mut pool = PeerPool::new(5);
        let entry = make_peer("10.0.0.1:6881");
        let id = pool.add(entry).unwrap();
        assert_eq!(pool.len(), 1);
        pool.remove(id).unwrap();
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn pool_full_rejected() {
        let mut pool = PeerPool::new(2);
        pool.add(make_peer("10.0.0.1:6881")).unwrap();
        pool.add(make_peer("10.0.0.2:6881")).unwrap();
        assert!(pool.add(make_peer("10.0.0.3:6881")).is_err());
    }

    #[test]
    fn duplicate_address_rejected() {
        let mut pool = PeerPool::new(5);
        pool.add(make_peer("10.0.0.1:6881")).unwrap();
        assert!(pool.add(make_peer("10.0.0.1:6881")).is_err());
    }

    #[test]
    fn remove_unknown_errors() {
        let mut pool = PeerPool::new(5);
        let fake = PeerId::new();
        assert!(pool.remove(fake).is_err());
    }

    #[test]
    fn known_addresses_excludes_removed() {
        let mut pool = PeerPool::new(5);
        let e1 = make_peer("10.0.0.1:6881");
        let e2 = make_peer("10.0.0.2:6881");
        let id1 = pool.add(e1).unwrap();
        pool.add(e2).unwrap();
        pool.remove(id1).unwrap();
        let addrs: Vec<_> = pool.known_addresses().collect();
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0], "10.0.0.2:6881".parse().unwrap());
    }

    #[test]
    fn is_full_flag() {
        let mut pool = PeerPool::new(1);
        assert!(!pool.is_full());
        pool.add(make_peer("10.0.0.1:6881")).unwrap();
        assert!(pool.is_full());
    }
}
