use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::error::TrackerError;

/// A peer address returned by a tracker.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Peer {
    pub addr: SocketAddr,
    /// Peer ID, if provided in non-compact mode.
    pub peer_id: Option<[u8; 20]>,
}

/// Compact IPv4 peer encoding: 4 bytes IP + 2 bytes port (BEP 23).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactPeer {
    pub addr: SocketAddr,
}

impl CompactPeer {
    pub fn from_bytes_v4(b: &[u8; 6]) -> Self {
        let ip = Ipv4Addr::new(b[0], b[1], b[2], b[3]);
        let port = u16::from_be_bytes([b[4], b[5]]);
        CompactPeer {
            addr: SocketAddr::new(IpAddr::V4(ip), port),
        }
    }

    pub fn from_bytes_v6(b: &[u8; 18]) -> Self {
        let ip = Ipv6Addr::from([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13],
            b[14], b[15],
        ]);
        let port = u16::from_be_bytes([b[16], b[17]]);
        CompactPeer {
            addr: SocketAddr::new(IpAddr::V6(ip), port),
        }
    }
}

/// Parse compact IPv4 peer list (BEP 23): 6-byte chunks.
pub fn parse_compact_peers_v4(bytes: &[u8]) -> Result<Vec<Peer>, TrackerError> {
    if !bytes.len().is_multiple_of(6) {
        return Err(TrackerError::ParseError(format!(
            "compact peers v4 length {} not multiple of 6",
            bytes.len()
        )));
    }
    Ok(bytes
        .as_chunks::<6>()
        .0
        .iter()
        .map(|arr| Peer {
            addr: CompactPeer::from_bytes_v4(arr).addr,
            peer_id: None,
        })
        .collect())
}

/// Parse compact IPv6 peer list: 18-byte chunks.
pub fn parse_compact_peers_v6(bytes: &[u8]) -> Result<Vec<Peer>, TrackerError> {
    if !bytes.len().is_multiple_of(18) {
        return Err(TrackerError::ParseError(format!(
            "compact peers v6 length {} not multiple of 18",
            bytes.len()
        )));
    }
    Ok(bytes
        .as_chunks::<18>()
        .0
        .iter()
        .map(|arr| Peer {
            addr: CompactPeer::from_bytes_v6(arr).addr,
            peer_id: None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_compact_v4() {
        // 127.0.0.1:6881
        let bytes = [127u8, 0, 0, 1, 0x1A, 0xE1];
        let peers = parse_compact_peers_v4(&bytes).unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].addr, "127.0.0.1:6881".parse().unwrap());
    }

    #[test]
    fn parse_multiple_compact_v4() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[10, 0, 0, 1, 0x1A, 0xE1]);
        bytes.extend_from_slice(&[192, 168, 1, 2, 0x6E, 0x80]);
        let peers = parse_compact_peers_v4(&bytes).unwrap();
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].addr.port(), 6881);
        assert_eq!(peers[1].addr.port(), 28288);
    }

    #[test]
    fn reject_invalid_compact_length() {
        let bytes = [127u8, 0, 0, 1, 0x1A]; // 5 bytes, not multiple of 6
        assert!(parse_compact_peers_v4(&bytes).is_err());
    }

    #[test]
    fn empty_compact_peers() {
        let peers = parse_compact_peers_v4(&[]).unwrap();
        assert!(peers.is_empty());
    }

    #[test]
    fn parse_compact_v6() {
        // ::1 port 6881
        let mut bytes = [0u8; 18];
        bytes[15] = 1; // ::1
        bytes[16] = 0x1A;
        bytes[17] = 0xE1;
        let peers = parse_compact_peers_v6(&bytes).unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].addr.port(), 6881);
    }
}
