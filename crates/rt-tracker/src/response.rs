use std::net::{IpAddr, SocketAddr};

use rt_bencode::{decode, BValue};

use crate::{
    error::TrackerError,
    peer::{parse_compact_peers_v4, parse_compact_peers_v6, Peer},
};

/// Current status of a tracker.
#[derive(Debug, Clone)]
pub enum TrackerStatus {
    NeverAnnounced,
    Announcing,
    Working,
    Warning(String),
    Error(TrackerError),
    Disabled,
}

impl PartialEq for TrackerStatus {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (TrackerStatus::NeverAnnounced, TrackerStatus::NeverAnnounced)
                | (TrackerStatus::Announcing, TrackerStatus::Announcing)
                | (TrackerStatus::Working, TrackerStatus::Working)
                | (TrackerStatus::Disabled, TrackerStatus::Disabled)
        ) || matches!((self, other), (TrackerStatus::Warning(a), TrackerStatus::Warning(b)) if a == b)
        // Error variants compare as equal regardless of inner value (for test convenience)
        || matches!((self, other), (TrackerStatus::Error(_), TrackerStatus::Error(_)))
    }
}

/// Parsed announce response from a tracker.
#[derive(Debug, Clone)]
pub struct AnnounceResponse {
    /// Seconds until next announce.
    pub interval: u32,
    /// Minimum seconds until next announce (optional, BEP 3).
    pub min_interval: Option<u32>,
    pub peers: Vec<Peer>,
    /// Optional tracker ID (re-sent on subsequent announces).
    /// Opaque bytes; BEP 3 does not require tracker IDs to be UTF-8.
    pub tracker_id: Option<Vec<u8>>,
    pub warning_message: Option<String>,
    pub complete: Option<u32>,
    pub incomplete: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrapeStats {
    pub complete: u32,
    pub downloaded: u32,
    pub incomplete: u32,
}

impl ScrapeStats {
    pub fn parse(bytes: &[u8], info_hash: &[u8]) -> Result<Self, TrackerError> {
        let val = decode(bytes).map_err(|e| TrackerError::ParseError(e.to_string()))?;
        if let Some(reason) = val.get(b"failure reason") {
            let msg = reason
                .as_bytes()
                .and_then(|b| std::str::from_utf8(b).ok())
                .unwrap_or("unknown failure")
                .to_owned();
            return Err(TrackerError::FailureReason(msg));
        }
        let files = val
            .get(b"files")
            .ok_or_else(|| TrackerError::ParseError("missing files".into()))?;
        let entry = match files {
            BValue::Dict(entries) => entries
                .iter()
                .find(|(key, _)| *key == info_hash)
                .map(|(_, value)| value)
                .ok_or_else(|| TrackerError::ParseError("scrape missing info_hash".into()))?,
            _ => return Err(TrackerError::ParseError("files is not a dict".into())),
        };
        Ok(ScrapeStats {
            complete: scrape_int(entry, b"complete")?,
            downloaded: scrape_int(entry, b"downloaded")?,
            incomplete: scrape_int(entry, b"incomplete")?,
        })
    }
}

fn scrape_int(entry: &BValue<'_>, key: &[u8]) -> Result<u32, TrackerError> {
    entry
        .get(key)
        .and_then(|value| value.as_int())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            TrackerError::ParseError(format!(
                "missing or invalid scrape field {}",
                String::from_utf8_lossy(key)
            ))
        })
}

impl AnnounceResponse {
    /// Parse a bencoded HTTP announce response.
    pub fn parse(bytes: &[u8]) -> Result<Self, TrackerError> {
        let val = decode(bytes).map_err(|e| TrackerError::ParseError(e.to_string()))?;

        // Check for failure reason first
        if let Some(reason) = val.get(b"failure reason") {
            let msg = reason
                .as_bytes()
                .and_then(|b| std::str::from_utf8(b).ok())
                .unwrap_or("unknown failure")
                .to_owned();
            return Err(TrackerError::FailureReason(msg));
        }

        // TNG-020: these were bare `as u32` casts -- a tracker sending a
        // negative or absurdly large bencoded integer (buggy or hostile)
        // would silently wrap into a huge or unrelated u32 instead of
        // being rejected. `interval` drives re-announce timing and hence
        // real network/tracker load, so an invalid value fails the whole
        // response, matching `scrape_int`'s existing checked pattern
        // below. The optional stats fields degrade to `None` instead --
        // failing an entire announce over a cosmetic field being garbage
        // would be needlessly fragile against a quirky-but-usable tracker.
        let interval = val
            .get(b"interval")
            .and_then(|v| v.as_int())
            .ok_or_else(|| TrackerError::ParseError("missing interval".into()))
            .and_then(|value| {
                u32::try_from(value).map_err(|_| {
                    TrackerError::ParseError(format!("interval out of range: {value}"))
                })
            })?;

        let min_interval = val
            .get(b"min interval")
            .and_then(|v| v.as_int())
            .and_then(|i| u32::try_from(i).ok());

        let warning_message = val
            .get(b"warning message")
            .and_then(|v| v.as_bytes())
            .and_then(|b| std::str::from_utf8(b).ok())
            .map(|s| s.to_owned());

        let tracker_id = val
            .get(b"tracker id")
            .and_then(|v| v.as_bytes())
            .map(ToOwned::to_owned);

        let complete = val
            .get(b"complete")
            .and_then(|v| v.as_int())
            .and_then(|i| u32::try_from(i).ok());
        let incomplete = val
            .get(b"incomplete")
            .and_then(|v| v.as_int())
            .and_then(|i| u32::try_from(i).ok());

        let peers = parse_peers_field(&val)?;

        Ok(AnnounceResponse {
            interval,
            min_interval,
            peers,
            tracker_id,
            warning_message,
            complete,
            incomplete,
        })
    }
}

fn parse_peers_field(val: &BValue<'_>) -> Result<Vec<Peer>, TrackerError> {
    let mut peers = match val.get(b"peers") {
        Some(BValue::Bytes(b)) => {
            // Compact IPv4 format (BEP 23).
            parse_compact_peers_v4(b)?
        }
        Some(BValue::List(entries)) => {
            // Non-compact (legacy) format.
            let mut peers = Vec::with_capacity(entries.len());
            for entry in entries {
                let ip = entry
                    .get(b"ip")
                    .and_then(|v| v.as_bytes())
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .ok_or_else(|| TrackerError::ParseError("peer missing ip".into()))?;
                let port = entry
                    .get(b"port")
                    .and_then(|v| v.as_int())
                    .ok_or_else(|| TrackerError::ParseError("peer missing port".into()))
                    .and_then(|port| {
                        u16::try_from(port).map_err(|_| {
                            TrackerError::ParseError(format!("invalid peer port: {port}"))
                        })
                    })?;
                let peer_id = entry
                    .get(b"peer id")
                    .and_then(|v| v.as_bytes())
                    .and_then(|b| {
                        if b.len() == 20 {
                            let mut arr = [0u8; 20];
                            arr.copy_from_slice(b);
                            Some(arr)
                        } else {
                            None
                        }
                    });
                let ip = ip.parse::<IpAddr>().map_err(|_| {
                    TrackerError::ParseError(format!("invalid peer address: {ip}:{port}"))
                })?;
                peers.push(Peer {
                    addr: SocketAddr::new(ip, port),
                    peer_id,
                });
            }
            peers
        }
        None => Vec::new(),
        Some(_) => {
            return Err(TrackerError::ParseError(
                "unexpected peers field type".into(),
            ))
        }
    };

    if let Some(peers6) = val.get(b"peers6") {
        let BValue::Bytes(bytes) = peers6 else {
            return Err(TrackerError::ParseError(
                "unexpected peers6 field type".into(),
            ));
        };
        // Compact IPv6 format (BEP 7).
        peers.extend(parse_compact_peers_v6(bytes)?);
    }

    Ok(peers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rt_bencode::{encode, BValue};

    fn make_response(interval: i64, peers_bytes: Option<&[u8]>, failure: Option<&str>) -> Vec<u8> {
        let mut pairs: Vec<(&[u8], BValue<'_>)> = Vec::new();
        if let Some(f) = failure {
            pairs.push((b"failure reason", BValue::Bytes(f.as_bytes())));
        } else {
            pairs.push((b"interval", BValue::Int(interval)));
            if let Some(p) = peers_bytes {
                pairs.push((b"peers", BValue::Bytes(p)));
            }
        }
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        encode(&BValue::Dict(pairs))
    }

    #[test]
    fn parse_compact_response() {
        // Two compact peers: 10.0.0.1:6881 and 192.168.1.2:6882
        let mut peer_bytes = Vec::new();
        peer_bytes.extend_from_slice(&[10, 0, 0, 1, 0x1A, 0xE1]);
        peer_bytes.extend_from_slice(&[192, 168, 1, 2, 0x1A, 0xE2]);
        let raw = make_response(1800, Some(&peer_bytes), None);

        let resp = AnnounceResponse::parse(&raw).unwrap();
        assert_eq!(resp.interval, 1800);
        assert_eq!(resp.peers.len(), 2);
        assert_eq!(resp.peers[0].addr.port(), 6881);
        assert_eq!(resp.peers[1].addr.port(), 6882);
    }

    #[test]
    fn parse_failure_reason() {
        let raw = make_response(0, None, Some("unregistered torrent"));
        let err = AnnounceResponse::parse(&raw).unwrap_err();
        assert!(matches!(err, TrackerError::FailureReason(ref s) if s.contains("unregistered")));
    }

    #[test]
    fn parse_empty_peers() {
        let raw = make_response(3600, Some(&[]), None);
        let resp = AnnounceResponse::parse(&raw).unwrap();
        assert!(resp.peers.is_empty());
    }

    #[test]
    fn parse_compact_ipv6_peers_field() {
        let mut peers6 = [0u8; 18];
        peers6[..16].copy_from_slice(&std::net::Ipv6Addr::LOCALHOST.octets());
        peers6[16..].copy_from_slice(&6881u16.to_be_bytes());
        let raw = encode(&BValue::Dict(vec![
            (b"interval".as_ref(), BValue::Int(1800)),
            (b"peers".as_ref(), BValue::Bytes(b"")),
            (b"peers6".as_ref(), BValue::Bytes(&peers6)),
        ]));

        let response = AnnounceResponse::parse(&raw).unwrap();

        assert_eq!(response.peers.len(), 1);
        assert_eq!(response.peers[0].addr, "[::1]:6881".parse().unwrap());
    }

    #[test]
    fn parse_noncompact_ipv6_peer() {
        let peer = BValue::Dict(vec![
            (b"ip".as_ref(), BValue::Bytes(b"2001:db8::1")),
            (b"port".as_ref(), BValue::Int(6881)),
        ]);
        let raw = encode(&BValue::Dict(vec![
            (b"interval".as_ref(), BValue::Int(1800)),
            (b"peers".as_ref(), BValue::List(vec![peer])),
        ]));

        let response = AnnounceResponse::parse(&raw).unwrap();

        assert_eq!(
            response.peers[0].addr,
            "[2001:db8::1]:6881".parse().unwrap()
        );
    }

    #[test]
    fn reject_noncompact_peer_port_out_of_range() {
        let peer = BValue::Dict(vec![
            (b"ip".as_ref(), BValue::Bytes(b"127.0.0.1")),
            (b"port".as_ref(), BValue::Int(65_536)),
        ]);
        let raw = encode(&BValue::Dict(vec![
            (b"interval".as_ref(), BValue::Int(1800)),
            (b"peers".as_ref(), BValue::List(vec![peer])),
        ]));

        assert!(matches!(
            AnnounceResponse::parse(&raw),
            Err(TrackerError::ParseError(message)) if message.contains("port")
        ));
    }

    #[test]
    fn parse_missing_interval() {
        // Response with no interval field
        let pairs: Vec<(&[u8], BValue<'_>)> = vec![(b"peers", BValue::Bytes(b""))];
        let raw = encode(&BValue::Dict(pairs));
        let result = AnnounceResponse::parse(&raw);
        assert!(matches!(result, Err(TrackerError::ParseError(_))));
    }

    #[test]
    fn parse_rejects_negative_interval() {
        // A negative interval used to be cast straight to u32 with `as`,
        // silently wrapping to a huge positive value instead of being
        // rejected -- a hostile or buggy tracker could use this to push a
        // client toward an absurd re-announce schedule.
        let raw = make_response(-1, Some(&[]), None);
        let result = AnnounceResponse::parse(&raw);
        assert!(matches!(result, Err(TrackerError::ParseError(_))));
    }

    #[test]
    fn parse_rejects_interval_overflowing_u32() {
        let raw = make_response(i64::from(u32::MAX) + 1, Some(&[]), None);
        let result = AnnounceResponse::parse(&raw);
        assert!(matches!(result, Err(TrackerError::ParseError(_))));
    }

    #[test]
    fn parse_treats_out_of_range_optional_stats_as_absent() {
        // min_interval/complete/incomplete are informational-only; an
        // invalid value in one of them should degrade to `None` rather
        // than either wrapping (the old `as u32` behavior) or failing the
        // whole announce response over a cosmetic field.
        let mut pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"interval", BValue::Int(1800)),
            (b"peers", BValue::Bytes(b"")),
            (b"min interval", BValue::Int(-5)),
            (b"complete", BValue::Int(-1)),
            (b"incomplete", BValue::Int(i64::from(u32::MAX) + 1)),
        ];
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(pairs));

        let resp = AnnounceResponse::parse(&raw).unwrap();

        assert_eq!(resp.interval, 1800);
        assert_eq!(resp.min_interval, None);
        assert_eq!(resp.complete, None);
        assert_eq!(resp.incomplete, None);
    }

    #[test]
    fn parse_preserves_opaque_tracker_id_bytes() {
        let raw = encode(&BValue::Dict(vec![
            (b"interval".as_ref(), BValue::Int(1800)),
            (b"peers".as_ref(), BValue::Bytes(b"")),
            (
                b"tracker id".as_ref(),
                BValue::Bytes(&[0x00, 0xff, 0x2d, 0x80]),
            ),
        ]));

        let response = AnnounceResponse::parse(&raw).unwrap();

        assert_eq!(response.tracker_id, Some(vec![0x00, 0xff, 0x2d, 0x80]));
    }

    #[test]
    fn parse_scrape_rejects_missing_info_hash() {
        let requested = [0x33u8; 20];
        let other = [0x44u8; 20];
        let entry = vec![
            (&b"complete"[..], BValue::Int(12)),
            (&b"downloaded"[..], BValue::Int(34)),
            (&b"incomplete"[..], BValue::Int(5)),
        ];
        let files = vec![(&other[..], BValue::Dict(entry))];
        let raw = encode(&BValue::Dict(vec![(&b"files"[..], BValue::Dict(files))]));

        let err = ScrapeStats::parse(&raw, &requested).unwrap_err();

        assert!(matches!(err, TrackerError::ParseError(_)));
    }

    #[test]
    fn parse_scrape_rejects_negative_counts() {
        let info_hash = [0x33u8; 20];
        let entry = vec![
            (&b"complete"[..], BValue::Int(-1)),
            (&b"downloaded"[..], BValue::Int(34)),
            (&b"incomplete"[..], BValue::Int(5)),
        ];
        let files = vec![(&info_hash[..], BValue::Dict(entry))];
        let raw = encode(&BValue::Dict(vec![(&b"files"[..], BValue::Dict(files))]));

        let err = ScrapeStats::parse(&raw, &info_hash).unwrap_err();

        assert!(matches!(err, TrackerError::ParseError(_)));
    }

    #[test]
    fn parse_warning_message() {
        let mut pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"interval", BValue::Int(1800)),
            (b"peers", BValue::Bytes(b"")),
            (b"warning message", BValue::Bytes(b"low peers")),
        ];
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(pairs));
        let resp = AnnounceResponse::parse(&raw).unwrap();
        assert_eq!(resp.warning_message.as_deref(), Some("low peers"));
    }

    #[test]
    fn parse_scrape_stats_for_info_hash() {
        let info_hash = [0x22u8; 20];
        let mut stats: Vec<(&[u8], BValue<'_>)> = vec![
            (b"complete", BValue::Int(12)),
            (b"downloaded", BValue::Int(34)),
            (b"incomplete", BValue::Int(5)),
        ];
        stats.sort_by(|a, b| a.0.cmp(b.0));
        let files: Vec<(&[u8], BValue<'_>)> = vec![(&info_hash, BValue::Dict(stats))];
        let raw = encode(&BValue::Dict(vec![(b"files", BValue::Dict(files))]));

        let scrape = ScrapeStats::parse(&raw, &info_hash).unwrap();

        assert_eq!(
            scrape,
            ScrapeStats {
                complete: 12,
                downloaded: 34,
                incomplete: 5
            }
        );
    }
}
