/// UDP tracker protocol codec (BEP 15).
///
/// Implements connect/announce message encoding and response parsing.
/// The actual UDP I/O is handled by the caller via tokio UdpSocket.
use rand::Rng;

use crate::{error::TrackerError, peer::parse_compact_peers_v4, request::AnnounceRequest, Peer};

pub const PROTOCOL_MAGIC: u64 = 0x41727101980;
pub const ACTION_CONNECT: u32 = 0;
pub const ACTION_ANNOUNCE: u32 = 1;

/// A UDP connect request (step 1 of BEP 15 handshake).
#[derive(Debug, Clone)]
pub struct UdpConnectRequest {
    pub transaction_id: u32,
}

impl UdpConnectRequest {
    pub fn new() -> Self {
        UdpConnectRequest {
            transaction_id: rand::thread_rng().gen(),
        }
    }

    pub fn encode(&self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf[0..8].copy_from_slice(&PROTOCOL_MAGIC.to_be_bytes());
        buf[8..12].copy_from_slice(&ACTION_CONNECT.to_be_bytes());
        buf[12..16].copy_from_slice(&self.transaction_id.to_be_bytes());
        buf
    }
}

impl Default for UdpConnectRequest {
    fn default() -> Self {
        Self::new()
    }
}

/// A UDP connect response.
#[derive(Debug, Clone)]
pub struct UdpConnectResponse {
    pub transaction_id: u32,
    pub connection_id: u64,
}

impl UdpConnectResponse {
    pub fn parse(buf: &[u8]) -> Result<Self, TrackerError> {
        if buf.len() < 16 {
            return Err(TrackerError::Udp(format!(
                "connect response too short: {} bytes",
                buf.len()
            )));
        }
        let action = u32::from_be_bytes(buf[0..4].try_into().unwrap());
        if action != ACTION_CONNECT {
            return Err(TrackerError::Udp(format!(
                "expected action 0 (connect), got {action}"
            )));
        }
        let transaction_id = u32::from_be_bytes(buf[4..8].try_into().unwrap());
        let connection_id = u64::from_be_bytes(buf[8..16].try_into().unwrap());
        Ok(UdpConnectResponse {
            transaction_id,
            connection_id,
        })
    }
}

/// A UDP announce request (step 2 of BEP 15).
#[derive(Debug, Clone)]
pub struct UdpAnnounceRequest {
    pub connection_id: u64,
    pub transaction_id: u32,
    pub req: AnnounceRequest,
    pub ip: u32,
    pub key: u32,
}

impl UdpAnnounceRequest {
    pub fn new(connection_id: u64, req: AnnounceRequest) -> Self {
        UdpAnnounceRequest {
            connection_id,
            transaction_id: rand::thread_rng().gen(),
            req,
            ip: 0,
            key: rand::thread_rng().gen(),
        }
    }

    /// Encode into 98-byte UDP datagram (BEP 15 format).
    pub fn encode(&self) -> Result<Vec<u8>, TrackerError> {
        let info_hash = match &self.req.info_hash {
            crate::request::InfoHash::V1(h) => h.as_slice().to_vec(),
            crate::request::InfoHash::V2(_) => {
                return Err(TrackerError::Udp(
                    "UDP tracker does not support v2 infohash".into(),
                ))
            }
        };

        let event_id: u32 = match self.req.event {
            crate::request::TrackerEvent::Empty => 0,
            crate::request::TrackerEvent::Completed => 1,
            crate::request::TrackerEvent::Started => 2,
            crate::request::TrackerEvent::Stopped => 3,
        };

        let numwant = self.req.numwant.unwrap_or(50);

        let mut buf = Vec::with_capacity(98);
        buf.extend_from_slice(&self.connection_id.to_be_bytes()); // 8
        buf.extend_from_slice(&ACTION_ANNOUNCE.to_be_bytes()); // 4
        buf.extend_from_slice(&self.transaction_id.to_be_bytes()); // 4
        buf.extend_from_slice(&info_hash); // 20
        buf.extend_from_slice(&self.req.peer_id); // 20
        buf.extend_from_slice(&self.req.downloaded.to_be_bytes()); // 8
        buf.extend_from_slice(&self.req.left.to_be_bytes()); // 8
        buf.extend_from_slice(&self.req.uploaded.to_be_bytes()); // 8
        buf.extend_from_slice(&event_id.to_be_bytes()); // 4
        buf.extend_from_slice(&self.ip.to_be_bytes()); // 4
        buf.extend_from_slice(&self.key.to_be_bytes()); // 4
        buf.extend_from_slice(&numwant.to_be_bytes()); // 4
        buf.extend_from_slice(&self.req.port.to_be_bytes()); // 2
                                                             // Total: 98 bytes
        assert_eq!(buf.len(), 98);
        Ok(buf)
    }
}

/// A UDP announce response.
#[derive(Debug, Clone)]
pub struct UdpAnnounceResponse {
    pub transaction_id: u32,
    pub interval: u32,
    pub leechers: u32,
    pub seeders: u32,
    pub peers: Vec<Peer>,
}

impl UdpAnnounceResponse {
    /// Parse a UDP announce response. Minimum 20 bytes header + compact peers.
    pub fn parse(buf: &[u8]) -> Result<Self, TrackerError> {
        if buf.len() < 20 {
            return Err(TrackerError::Udp(format!(
                "announce response too short: {} bytes",
                buf.len()
            )));
        }
        let action = u32::from_be_bytes(buf[0..4].try_into().unwrap());
        if action != ACTION_ANNOUNCE {
            return Err(TrackerError::Udp(format!(
                "expected action 1 (announce), got {action}"
            )));
        }
        let transaction_id = u32::from_be_bytes(buf[4..8].try_into().unwrap());
        let interval = u32::from_be_bytes(buf[8..12].try_into().unwrap());
        let leechers = u32::from_be_bytes(buf[12..16].try_into().unwrap());
        let seeders = u32::from_be_bytes(buf[16..20].try_into().unwrap());
        let peers = parse_compact_peers_v4(&buf[20..])?;
        Ok(UdpAnnounceResponse {
            transaction_id,
            interval,
            leechers,
            seeders,
            peers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{InfoHash, TrackerEvent};

    fn test_announce_req() -> AnnounceRequest {
        AnnounceRequest {
            info_hash: InfoHash::V1([0xABu8; 20]),
            peer_id: [0x2Du8; 20],
            port: 6881,
            uploaded: 0,
            downloaded: 0,
            left: 1024,
            event: TrackerEvent::Started,
            compact: true,
            numwant: Some(50),
        }
    }

    #[test]
    fn connect_request_encodes_magic() {
        let req = UdpConnectRequest::new();
        let buf = req.encode();
        assert_eq!(buf.len(), 16);
        let magic = u64::from_be_bytes(buf[0..8].try_into().unwrap());
        assert_eq!(magic, PROTOCOL_MAGIC);
        let action = u32::from_be_bytes(buf[8..12].try_into().unwrap());
        assert_eq!(action, ACTION_CONNECT);
    }

    #[test]
    fn connect_response_parse() {
        let mut buf = [0u8; 16];
        buf[0..4].copy_from_slice(&ACTION_CONNECT.to_be_bytes());
        let txn: u32 = 0xDEADBEEF;
        buf[4..8].copy_from_slice(&txn.to_be_bytes());
        let conn_id: u64 = 0x1234567890ABCDEF;
        buf[8..16].copy_from_slice(&conn_id.to_be_bytes());
        let resp = UdpConnectResponse::parse(&buf).unwrap();
        assert_eq!(resp.transaction_id, txn);
        assert_eq!(resp.connection_id, conn_id);
    }

    #[test]
    fn connect_response_too_short() {
        assert!(UdpConnectResponse::parse(&[0u8; 10]).is_err());
    }

    #[test]
    fn announce_request_encodes_98_bytes() {
        let req = UdpAnnounceRequest::new(0x12345678, test_announce_req());
        let encoded = req.encode().unwrap();
        assert_eq!(encoded.len(), 98);
    }

    #[test]
    fn announce_request_event_started_is_2() {
        let req = UdpAnnounceRequest::new(0, test_announce_req());
        let encoded = req.encode().unwrap();
        // Event is at byte offset 80 (8+4+4+20+20+8+8+8=80)
        let event = u32::from_be_bytes(encoded[80..84].try_into().unwrap());
        assert_eq!(event, 2); // started = 2
    }

    #[test]
    fn announce_response_parse() {
        let mut buf = vec![0u8; 20];
        // action = 1 (announce)
        buf[0..4].copy_from_slice(&ACTION_ANNOUNCE.to_be_bytes());
        let txn: u32 = 0xCAFEBABE;
        buf[4..8].copy_from_slice(&txn.to_be_bytes());
        // interval = 1800
        buf[8..12].copy_from_slice(&1800u32.to_be_bytes());
        // leechers = 5, seeders = 10
        buf[12..16].copy_from_slice(&5u32.to_be_bytes());
        buf[16..20].copy_from_slice(&10u32.to_be_bytes());
        // Two compact peers
        buf.extend_from_slice(&[10, 0, 0, 1, 0x1A, 0xE1]);
        buf.extend_from_slice(&[10, 0, 0, 2, 0x1A, 0xE2]);

        let resp = UdpAnnounceResponse::parse(&buf).unwrap();
        assert_eq!(resp.transaction_id, txn);
        assert_eq!(resp.interval, 1800);
        assert_eq!(resp.leechers, 5);
        assert_eq!(resp.seeders, 10);
        assert_eq!(resp.peers.len(), 2);
    }

    #[test]
    fn announce_response_too_short() {
        assert!(UdpAnnounceResponse::parse(&[0u8; 10]).is_err());
    }

    #[test]
    fn v2_infohash_rejected_for_udp() {
        let mut req = test_announce_req();
        req.info_hash = InfoHash::V2([0xCDu8; 32]);
        let udp_req = UdpAnnounceRequest::new(0, req);
        assert!(udp_req.encode().is_err());
    }
}
