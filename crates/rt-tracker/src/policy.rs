/// BEP 27 private torrent policy.
///
/// If a torrent is private (private=1 in info dict):
/// - DHT must not be used
/// - PEX must not be sent/accepted
/// - LSD must not be used
/// - Only peers from the private tracker are allowed
/// - Tracker switching follows BEP 12 but with additional isolation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrivatePolicy {
    pub is_private: bool,
}

impl PrivatePolicy {
    pub fn private() -> Self {
        PrivatePolicy { is_private: true }
    }

    pub fn public() -> Self {
        PrivatePolicy { is_private: false }
    }

    /// DHT is allowed only for non-private torrents.
    pub fn dht_allowed(&self) -> bool {
        !self.is_private
    }

    /// PEX extension is allowed only for non-private torrents.
    pub fn pex_allowed(&self) -> bool {
        !self.is_private
    }

    /// Local Service Discovery is allowed only for non-private torrents.
    pub fn lsd_allowed(&self) -> bool {
        !self.is_private
    }

    /// Whether the peer was obtained from a source that is allowed for this torrent.
    pub fn peer_source_allowed(&self, from_tracker: bool) -> bool {
        if self.is_private {
            from_tracker // private torrents: only tracker peers
        } else {
            true // public torrents: any source
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_disables_dht_pex_lsd() {
        let p = PrivatePolicy::private();
        assert!(!p.dht_allowed());
        assert!(!p.pex_allowed());
        assert!(!p.lsd_allowed());
    }

    #[test]
    fn public_allows_all() {
        let p = PrivatePolicy::public();
        assert!(p.dht_allowed());
        assert!(p.pex_allowed());
        assert!(p.lsd_allowed());
    }

    #[test]
    fn private_only_allows_tracker_peers() {
        let p = PrivatePolicy::private();
        assert!(p.peer_source_allowed(true));
        assert!(!p.peer_source_allowed(false)); // DHT/PEX peer rejected
    }

    #[test]
    fn public_allows_any_peer_source() {
        let p = PrivatePolicy::public();
        assert!(p.peer_source_allowed(true));
        assert!(p.peer_source_allowed(false));
    }
}
