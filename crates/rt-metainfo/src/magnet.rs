use std::collections::HashSet;
use std::net::SocketAddr;

use url::Url;

use crate::{error::MetainfoError, types::MagnetLink};

const MAX_MAGNET_BYTES: usize = 4 * 1024 * 1024;
const MAX_MAGNET_NAME_BYTES: usize = 4096;
const MAX_MAGNET_TRACKERS: usize = 4096;
const MAX_MAGNET_TRACKER_BYTES: usize = 8192;
const MAX_MAGNET_TRACKER_TOTAL_BYTES: usize = 1024 * 1024;
const MAX_MAGNET_PEER_ADDRESSES: usize = 256;

pub fn parse_magnet(input: &str) -> Result<MagnetLink, MetainfoError> {
    if input.len() > MAX_MAGNET_BYTES {
        return Err(MetainfoError::LimitExceeded {
            field: "magnet bytes",
            limit: MAX_MAGNET_BYTES,
        });
    }
    let url = Url::parse(input).map_err(|e| MetainfoError::InvalidMagnet(e.to_string()))?;
    if url.scheme() != "magnet" {
        return Err(MetainfoError::InvalidMagnet(
            "scheme must be magnet".to_owned(),
        ));
    }

    let mut info_hash_v1 = None;
    let mut info_hash_v2 = None;
    let mut display_name = None;
    let mut trackers = Vec::new();
    let mut seen_trackers = HashSet::new();
    let mut tracker_bytes: usize = 0;
    let mut peer_addresses = Vec::new();
    let mut seen_peer_addresses = HashSet::new();

    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "xt" => parse_exact_topic(&value, &mut info_hash_v1, &mut info_hash_v2)?,
            "dn" => {
                let name = value.trim();
                if name.len() > MAX_MAGNET_NAME_BYTES {
                    return Err(MetainfoError::LimitExceeded {
                        field: "magnet display name bytes",
                        limit: MAX_MAGNET_NAME_BYTES,
                    });
                }
                if !name.is_empty() {
                    display_name = Some(name.to_owned());
                }
            }
            "tr" => {
                let tracker = value.trim();
                if tracker.len() > MAX_MAGNET_TRACKER_BYTES {
                    return Err(MetainfoError::LimitExceeded {
                        field: "magnet tracker url bytes",
                        limit: MAX_MAGNET_TRACKER_BYTES,
                    });
                }
                if !tracker.is_empty() && seen_trackers.insert(tracker.to_owned()) {
                    if trackers.len() >= MAX_MAGNET_TRACKERS {
                        return Err(MetainfoError::LimitExceeded {
                            field: "magnet tracker urls",
                            limit: MAX_MAGNET_TRACKERS,
                        });
                    }
                    tracker_bytes = tracker_bytes.saturating_add(tracker.len());
                    if tracker_bytes > MAX_MAGNET_TRACKER_TOTAL_BYTES {
                        return Err(MetainfoError::LimitExceeded {
                            field: "magnet tracker url bytes",
                            limit: MAX_MAGNET_TRACKER_TOTAL_BYTES,
                        });
                    }
                    trackers.push(tracker.to_owned());
                }
            }
            "x.pe" => {
                let peer = value.trim();
                let peer = peer.parse::<SocketAddr>().map_err(|error| {
                    MetainfoError::InvalidMagnet(format!(
                        "invalid x.pe peer address {peer:?}: {error}"
                    ))
                })?;
                if peer.port() == 0 {
                    return Err(MetainfoError::InvalidMagnet(
                        "x.pe peer port must be non-zero".to_owned(),
                    ));
                }
                if seen_peer_addresses.insert(peer) {
                    if peer_addresses.len() >= MAX_MAGNET_PEER_ADDRESSES {
                        return Err(MetainfoError::LimitExceeded {
                            field: "magnet peer addresses",
                            limit: MAX_MAGNET_PEER_ADDRESSES,
                        });
                    }
                    peer_addresses.push(peer);
                }
            }
            _ => {}
        }
    }

    if info_hash_v1.is_none() && info_hash_v2.is_none() {
        return Err(MetainfoError::InvalidMagnet(
            "missing btih or btmh exact topic".to_owned(),
        ));
    }

    Ok(MagnetLink {
        info_hash_v1,
        info_hash_v2,
        display_name,
        trackers,
        peer_addresses,
    })
}

fn parse_exact_topic(
    value: &str,
    info_hash_v1: &mut Option<[u8; 20]>,
    info_hash_v2: &mut Option<[u8; 32]>,
) -> Result<(), MetainfoError> {
    if let Some(hash) = value.strip_prefix("urn:btih:") {
        *info_hash_v1 = Some(parse_btih(hash)?);
        return Ok(());
    }
    if let Some(multihash) = value.strip_prefix("urn:btmh:") {
        *info_hash_v2 = Some(parse_btmh(multihash)?);
    }
    Ok(())
}

fn parse_btih(hash: &str) -> Result<[u8; 20], MetainfoError> {
    if hash.len() == 40 {
        let bytes =
            hex::decode(hash).map_err(|e| MetainfoError::UnsupportedMagnetHash(e.to_string()))?;
        return bytes
            .try_into()
            .map_err(|_| MetainfoError::UnsupportedMagnetHash(hash.to_owned()));
    }
    if hash.len() == 32 {
        return decode_base32_btih(hash);
    }
    Err(MetainfoError::UnsupportedMagnetHash(
        "btih must be 40-character hex or 32-character base32".to_owned(),
    ))
}

fn decode_base32_btih(hash: &str) -> Result<[u8; 20], MetainfoError> {
    let mut out = [0u8; 20];
    let mut buffer = 0u32;
    let mut bits = 0u8;
    let mut written = 0usize;

    for byte in hash.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a',
            b'2'..=b'7' => byte - b'2' + 26,
            _ => {
                return Err(MetainfoError::UnsupportedMagnetHash(format!(
                    "invalid base32 btih character: {byte:#04x}"
                )));
            }
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        while bits >= 8 {
            bits -= 8;
            if written >= out.len() {
                return Err(MetainfoError::UnsupportedMagnetHash(
                    "base32 btih decoded too many bytes".to_owned(),
                ));
            }
            out[written] = (buffer >> bits) as u8;
            written += 1;
            buffer &= (1 << bits) - 1;
        }
    }

    if written != out.len() || bits != 0 {
        return Err(MetainfoError::UnsupportedMagnetHash(
            "base32 btih did not decode to 20 bytes".to_owned(),
        ));
    }
    Ok(out)
}

fn parse_btmh(multihash: &str) -> Result<[u8; 32], MetainfoError> {
    let bytes =
        hex::decode(multihash).map_err(|e| MetainfoError::UnsupportedMagnetHash(e.to_string()))?;
    if bytes.len() == 34 && bytes[0] == 0x12 && bytes[1] == 0x20 {
        return bytes[2..]
            .try_into()
            .map_err(|_| MetainfoError::UnsupportedMagnetHash(multihash.to_owned()));
    }
    Err(MetainfoError::UnsupportedMagnetHash(
        "only sha2-256 btmh multihashes are currently supported".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v1_magnet_with_name_and_trackers() {
        let magnet = parse_magnet(
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Ubuntu&tr=http%3A%2F%2Ftracker%2Fannounce&tr=http%3A%2F%2Ftracker%2Fannounce",
        )
        .unwrap();

        assert_eq!(magnet.info_hash_v1.unwrap()[0], 0x01);
        assert_eq!(magnet.display_name.as_deref(), Some("Ubuntu"));
        assert_eq!(magnet.trackers, vec!["http://tracker/announce"]);
    }

    #[test]
    fn parses_v2_btmh_magnet() {
        let magnet = parse_magnet(
            "magnet:?xt=urn:btmh:1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap();

        assert_eq!(magnet.info_hash_v2.unwrap(), [0xaa; 32]);
    }

    #[test]
    fn parses_and_deduplicates_bep9_direct_peers() {
        let magnet = parse_magnet(
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&x.pe=127.0.0.1%3A6881&x.pe=%5B%3A%3A1%5D%3A6881&x.pe=127.0.0.1%3A6881",
        )
        .unwrap();

        assert_eq!(
            magnet.peer_addresses,
            vec![
                "127.0.0.1:6881".parse::<SocketAddr>().unwrap(),
                "[::1]:6881".parse::<SocketAddr>().unwrap(),
            ]
        );
    }

    #[test]
    fn rejects_invalid_bep9_direct_peer() {
        assert!(parse_magnet(
            "magnet:?xt=urn:btih:0000000000000000000000000000000000000000&x.pe=127.0.0.1%3A0"
        )
        .is_err());
        assert!(parse_magnet(
            "magnet:?xt=urn:btih:0000000000000000000000000000000000000000&x.pe=localhost%3A6881"
        )
        .is_err());
    }

    #[test]
    fn rejects_too_many_bep9_direct_peers() {
        let mut input = format!("magnet:?xt=urn:btih:{}", "0".repeat(40));
        for index in 0..=MAX_MAGNET_PEER_ADDRESSES {
            input.push_str("&x.pe=192.0.2.1:");
            input.push_str(&(10_000 + index).to_string());
        }
        assert!(matches!(
            parse_magnet(&input),
            Err(MetainfoError::LimitExceeded {
                field: "magnet peer addresses",
                limit: MAX_MAGNET_PEER_ADDRESSES
            })
        ));
    }

    #[test]
    fn rejects_missing_exact_topic() {
        assert!(parse_magnet("magnet:?dn=nope").is_err());
    }

    #[test]
    fn parses_base32_btih() {
        let magnet = parse_magnet("magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        assert_eq!(magnet.info_hash_v1.unwrap(), [0; 20]);
    }

    #[test]
    fn parses_lowercase_base32_btih() {
        let magnet = parse_magnet("magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        assert_eq!(magnet.info_hash_v1.unwrap(), [0; 20]);
    }

    #[test]
    fn rejects_invalid_base32_btih() {
        assert!(parse_magnet("magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA1").is_err());
    }

    #[test]
    fn rejects_oversized_magnet_input() {
        let input = format!(
            "magnet:?xt=urn:btih:{}&dn={}",
            "0".repeat(40),
            "n".repeat(MAX_MAGNET_BYTES)
        );
        assert!(matches!(
            parse_magnet(&input),
            Err(MetainfoError::LimitExceeded {
                field: "magnet bytes",
                limit: MAX_MAGNET_BYTES
            })
        ));
    }

    #[test]
    fn rejects_oversized_magnet_display_name() {
        let input = format!(
            "magnet:?xt=urn:btih:{}&dn={}",
            "0".repeat(40),
            "n".repeat(MAX_MAGNET_NAME_BYTES + 1)
        );
        assert!(matches!(
            parse_magnet(&input),
            Err(MetainfoError::LimitExceeded {
                field: "magnet display name bytes",
                limit: MAX_MAGNET_NAME_BYTES
            })
        ));
    }

    #[test]
    fn rejects_oversized_magnet_tracker() {
        let input = format!(
            "magnet:?xt=urn:btih:{}&tr={}",
            "0".repeat(40),
            "t".repeat(MAX_MAGNET_TRACKER_BYTES + 1)
        );
        assert!(matches!(
            parse_magnet(&input),
            Err(MetainfoError::LimitExceeded {
                field: "magnet tracker url bytes",
                limit: MAX_MAGNET_TRACKER_BYTES
            })
        ));
    }

    #[test]
    fn rejects_too_many_magnet_trackers() {
        let mut input = format!("magnet:?xt=urn:btih:{}", "0".repeat(40));
        for index in 0..=MAX_MAGNET_TRACKERS {
            input.push_str("&tr=tracker");
            input.push_str(&index.to_string());
        }
        assert!(matches!(
            parse_magnet(&input),
            Err(MetainfoError::LimitExceeded {
                field: "magnet tracker urls",
                limit: MAX_MAGNET_TRACKERS
            })
        ));
    }

    #[test]
    fn rejects_aggregate_magnet_tracker_bytes() {
        let mut input = format!("magnet:?xt=urn:btih:{}", "0".repeat(40));
        for index in 0..=MAX_MAGNET_TRACKER_TOTAL_BYTES / MAX_MAGNET_TRACKER_BYTES {
            let tracker = format!("{}{}", "t".repeat(MAX_MAGNET_TRACKER_BYTES - 4), index);
            input.push_str("&tr=");
            input.push_str(&tracker);
        }
        assert!(matches!(
            parse_magnet(&input),
            Err(MetainfoError::LimitExceeded {
                field: "magnet tracker url bytes",
                limit: MAX_MAGNET_TRACKER_TOTAL_BYTES
            })
        ));
    }
}
