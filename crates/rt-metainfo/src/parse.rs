use sha1::{Digest, Sha1};
use sha2::Sha256;

use rt_bencode::{decode_torrent_info_span, BValue};
use rt_path::SafeRelPath;

use crate::{
    error::MetainfoError,
    types::{TorrentFileV1, TorrentFileV2, TorrentMeta, TorrentMetaV1, TorrentMetaV2},
};

/// Parse a `.torrent` file from raw bytes. Handles v1, v2 (BEP 52), and hybrid.
pub fn parse_torrent(raw: &[u8]) -> Result<TorrentMeta, MetainfoError> {
    let (val, info_span) = decode_torrent_info_span(raw)?;

    let root = match &val {
        BValue::Dict(_) => &val,
        _ => return Err(MetainfoError::MissingField("root dict")),
    };

    let info = root
        .get(b"info")
        .ok_or(MetainfoError::MissingField("info"))?;

    let info_bytes = if let Some(span) = info_span {
        &raw[span]
    } else {
        return Err(MetainfoError::MissingField("info span"));
    };

    let meta_version = info.get(b"meta version").and_then(|v| v.as_int());
    let has_file_tree = info.get(b"file tree").is_some();
    let has_pieces = info.get(b"pieces").is_some();

    let is_v2 = meta_version == Some(2) && has_file_tree;

    let announce = parse_announce(root);
    let announce_list = parse_announce_list(root);

    let name = get_string(info, b"name", "name")?;
    if name.is_empty() {
        return Err(MetainfoError::ZeroLengthName);
    }

    let piece_length = get_int(info, b"piece length", "piece length")? as u64;
    if piece_length == 0 || piece_length & (piece_length - 1) != 0 {
        return Err(MetainfoError::InvalidPieceLength(piece_length));
    }

    let private = info
        .get(b"private")
        .and_then(|v| v.as_int())
        .map(|i| i == 1)
        .unwrap_or(false);

    if is_v2 && has_pieces {
        // Hybrid: compute both infohashes
        let info_hash_v1: [u8; 20] = {
            let mut h = Sha1::new();
            h.update(info_bytes);
            h.finalize().into()
        };
        let info_hash_v2: [u8; 32] = {
            let mut h = Sha256::new();
            h.update(info_bytes);
            h.finalize().into()
        };

        let pieces_bytes = get_bytes(info, b"pieces", "pieces")?;
        if pieces_bytes.len() % 20 != 0 {
            return Err(MetainfoError::InvalidPiecesLength(pieces_bytes.len()));
        }
        let pieces: Vec<[u8; 20]> = pieces_bytes
            .chunks_exact(20)
            .map(|c| c.try_into().unwrap())
            .collect();

        let files_v1 = parse_files_v1(info, &name)?;
        let files_v2 = parse_file_tree(info, &name)?;

        return Ok(TorrentMeta::Hybrid(
            TorrentMetaV1 {
                info_hash: info_hash_v1,
                announce: announce.clone(),
                announce_list: announce_list.clone(),
                name: name.clone(),
                piece_length,
                pieces,
                files: files_v1,
                private,
                raw: raw.to_vec(),
            },
            TorrentMetaV2 {
                info_hash_v2,
                announce,
                announce_list,
                name,
                piece_length,
                files: files_v2,
                private,
                raw: raw.to_vec(),
            },
        ));
    }

    if is_v2 {
        // Pure v2
        let info_hash_v2: [u8; 32] = {
            let mut h = Sha256::new();
            h.update(info_bytes);
            h.finalize().into()
        };
        let files_v2 = parse_file_tree(info, &name)?;
        return Ok(TorrentMeta::V2(TorrentMetaV2 {
            info_hash_v2,
            announce,
            announce_list,
            name,
            piece_length,
            files: files_v2,
            private,
            raw: raw.to_vec(),
        }));
    }

    // v1
    let info_hash: [u8; 20] = {
        let mut h = Sha1::new();
        h.update(info_bytes);
        h.finalize().into()
    };
    let pieces_bytes = get_bytes(info, b"pieces", "pieces")?;
    if pieces_bytes.len() % 20 != 0 {
        return Err(MetainfoError::InvalidPiecesLength(pieces_bytes.len()));
    }
    let pieces: Vec<[u8; 20]> = pieces_bytes
        .chunks_exact(20)
        .map(|c| c.try_into().unwrap())
        .collect();

    let files = parse_files_v1(info, &name)?;

    Ok(TorrentMeta::V1(TorrentMetaV1 {
        info_hash,
        announce,
        announce_list,
        name,
        piece_length,
        pieces,
        files,
        private,
        raw: raw.to_vec(),
    }))
}

/// Parse a v2 `file tree` dict into a flat list of files.
/// BEP 52 file tree: nested dicts where leaves have `{"": {"length": N, "pieces root": <bytes>}}`.
fn parse_file_tree(
    info: &BValue<'_>,
    torrent_name: &str,
) -> Result<Vec<TorrentFileV2>, MetainfoError> {
    let file_tree = info
        .get(b"file tree")
        .ok_or(MetainfoError::MissingField("file tree"))?;

    let mut files = Vec::new();
    let mut offset = 0u64;
    walk_file_tree(file_tree, &[torrent_name], &mut files, &mut offset)?;

    if files.is_empty() {
        return Err(MetainfoError::MissingField("file tree (empty)"));
    }
    Ok(files)
}

fn walk_file_tree<'a>(
    node: &BValue<'a>,
    path_components: &[&str],
    out: &mut Vec<TorrentFileV2>,
    offset: &mut u64,
) -> Result<(), MetainfoError> {
    let dict = match node {
        BValue::Dict(pairs) => pairs,
        _ => return Err(MetainfoError::InvalidFieldType("file tree node")),
    };

    // Leaf: has empty-string key ""
    if let Some(leaf) = node.get(b"") {
        let length = get_int(leaf, b"length", "file tree length")? as u64;
        let pieces_root_bytes = get_bytes(leaf, b"pieces root", "pieces root")?;
        if pieces_root_bytes.len() != 32 {
            return Err(MetainfoError::InvalidFieldType(
                "pieces root (must be 32 bytes)",
            ));
        }
        let pieces_root: [u8; 32] = pieces_root_bytes.try_into().unwrap();

        let components: Vec<String> = path_components.iter().map(|s| s.to_string()).collect();
        let path = SafeRelPath::from_components(&components, false)?;

        let index = out.len() as u32;
        out.push(TorrentFileV2 {
            index,
            length,
            path,
            offset: *offset,
            pieces_root,
        });
        *offset += length;
        return Ok(());
    }

    // Interior node: recurse into each key (sorted dict)
    for (key, child) in dict {
        if key.is_empty() {
            continue;
        }
        let component =
            std::str::from_utf8(key).map_err(|_| MetainfoError::InvalidUtf8("file tree key"))?;
        let mut new_path: Vec<&str> = path_components.to_vec();
        new_path.push(component);
        walk_file_tree(child, &new_path, out, offset)?;
    }
    Ok(())
}

fn parse_announce(root: &BValue<'_>) -> Option<String> {
    root.get(b"announce")
        .and_then(|v| v.as_bytes())
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(|s| s.to_owned())
}

fn parse_files_v1(info: &BValue<'_>, name: &str) -> Result<Vec<TorrentFileV1>, MetainfoError> {
    if let Some(BValue::List(file_list)) = info.get(b"files") {
        // Multi-file torrent: name is the root directory
        let mut offset = 0u64;
        let mut files = Vec::with_capacity(file_list.len());
        for (idx, entry) in file_list.iter().enumerate() {
            let length = get_int(entry, b"length", "file length")? as u64;
            let path_list = match entry.get(b"path") {
                Some(BValue::List(parts)) => parts,
                _ => return Err(MetainfoError::MissingField("file path")),
            };
            let mut components: Vec<String> = vec![name.to_owned()];
            for part in path_list {
                let s = match part {
                    BValue::Bytes(b) => std::str::from_utf8(b)
                        .map_err(|_| MetainfoError::InvalidUtf8("path component"))?
                        .to_owned(),
                    _ => return Err(MetainfoError::InvalidFieldType("path component")),
                };
                components.push(s);
            }
            let path = SafeRelPath::from_components(&components, false)?;
            files.push(TorrentFileV1 {
                index: idx as u32,
                length,
                path,
                offset,
            });
            offset += length;
        }
        Ok(files)
    } else {
        // Single-file torrent
        let length = get_int(info, b"length", "length")? as u64;
        let path = SafeRelPath::from_name(name, false)?;
        Ok(vec![TorrentFileV1 {
            index: 0,
            length,
            path,
            offset: 0,
        }])
    }
}

fn parse_announce_list(root: &BValue<'_>) -> Vec<Vec<String>> {
    let Some(BValue::List(tiers)) = root.get(b"announce-list") else {
        return Vec::new();
    };
    tiers
        .iter()
        .filter_map(|tier| match tier {
            BValue::List(urls) => {
                let tier_urls: Vec<String> = urls
                    .iter()
                    .filter_map(|u| u.as_bytes())
                    .filter_map(|b| std::str::from_utf8(b).ok())
                    .map(|s| s.to_owned())
                    .collect();
                if tier_urls.is_empty() {
                    None
                } else {
                    Some(tier_urls)
                }
            }
            _ => None,
        })
        .collect()
}

fn get_bytes<'a>(
    dict: &'a BValue<'_>,
    key: &[u8],
    field: &'static str,
) -> Result<&'a [u8], MetainfoError> {
    match dict.get(key) {
        Some(BValue::Bytes(b)) => Ok(b),
        Some(_) => Err(MetainfoError::InvalidFieldType(field)),
        None => Err(MetainfoError::MissingField(field)),
    }
}

fn get_string(dict: &BValue<'_>, key: &[u8], field: &'static str) -> Result<String, MetainfoError> {
    let b = get_bytes(dict, key, field)?;
    std::str::from_utf8(b)
        .map(|s| s.to_owned())
        .map_err(|_| MetainfoError::InvalidUtf8(field))
}

fn get_int(dict: &BValue<'_>, key: &[u8], field: &'static str) -> Result<i64, MetainfoError> {
    match dict.get(key) {
        Some(BValue::Int(n)) => Ok(*n),
        Some(_) => Err(MetainfoError::InvalidFieldType(field)),
        None => Err(MetainfoError::MissingField(field)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rt_bencode::encode;
    use rt_bencode::BValue;

    fn make_pieces(n: usize) -> Vec<u8> {
        vec![0u8; n * 20]
    }

    fn single_file_torrent(
        name: &str,
        length: i64,
        piece_length: i64,
        private: Option<i64>,
    ) -> Vec<u8> {
        let pieces_data = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(length)),
            (b"name", BValue::Bytes(name.as_bytes())),
            (b"piece length", BValue::Int(piece_length)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        if let Some(p) = private {
            info_pairs.push((b"private", BValue::Int(p)));
        }
        // bencode dict keys must be sorted
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let info = BValue::Dict(info_pairs);

        let mut pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (
                b"announce",
                BValue::Bytes(b"http://tracker.example.com/announce"),
            ),
            (b"info", info),
        ];
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        encode(&BValue::Dict(pairs))
    }

    fn multi_file_torrent(name: &str, files: &[(&str, i64)]) -> Vec<u8> {
        let file_entries: Vec<BValue<'_>> = files
            .iter()
            .map(|(fname, len)| {
                let mut pairs: Vec<(&[u8], BValue<'_>)> = vec![
                    (b"length", BValue::Int(*len)),
                    (b"path", BValue::List(vec![BValue::Bytes(fname.as_bytes())])),
                ];
                pairs.sort_by(|a, b| a.0.cmp(b.0));
                BValue::Dict(pairs)
            })
            .collect();

        let pieces_data = make_pieces(2);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"files", BValue::List(file_entries)),
            (b"name", BValue::Bytes(name.as_bytes())),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let info = BValue::Dict(info_pairs);

        let mut pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (
                b"announce",
                BValue::Bytes(b"http://tracker.example.com/announce"),
            ),
            (b"info", info),
        ];
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        encode(&BValue::Dict(pairs))
    }

    #[test]
    fn parse_single_file() {
        let raw = single_file_torrent("test.bin", 1024, 512 * 1024, None);
        let meta = parse_torrent(&raw).unwrap();
        let TorrentMeta::V1(m) = meta else {
            panic!("expected V1")
        };
        assert_eq!(m.name, "test.bin");
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].length, 1024);
        assert_eq!(m.total_length(), 1024);
        assert!(m.is_single_file());
        assert!(!m.private);
        assert_eq!(m.info_hash.len(), 20);
    }

    #[test]
    fn parse_multi_file() {
        let raw = multi_file_torrent("mydir", &[("a.txt", 100), ("b.txt", 200)]);
        let meta = parse_torrent(&raw).unwrap();
        let TorrentMeta::V1(m) = meta else {
            panic!("expected V1")
        };
        assert_eq!(m.files.len(), 2);
        assert_eq!(m.files[0].offset, 0);
        assert_eq!(m.files[1].offset, 100);
        assert_eq!(m.total_length(), 300);
        assert!(!m.is_single_file());
    }

    #[test]
    fn parse_private_flag() {
        let raw = single_file_torrent("priv.bin", 512, 512 * 1024, Some(1));
        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert!(m.private);
    }

    #[test]
    fn infohash_stable_across_parses() {
        let raw = single_file_torrent("stable.bin", 1024, 512 * 1024, None);
        let TorrentMeta::V1(m1) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        let TorrentMeta::V1(m2) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert_eq!(m1.info_hash, m2.info_hash);
    }

    #[test]
    fn reject_path_traversal() {
        // Build a multi-file torrent with a path component ".."
        let pieces_data = make_pieces(1);
        let file_entry = BValue::Dict({
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"length", BValue::Int(100)),
                (
                    b"path",
                    BValue::List(vec![BValue::Bytes(b".."), BValue::Bytes(b"evil.sh")]),
                ),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        });
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"files", BValue::List(vec![file_entry])),
            (b"name", BValue::Bytes(b"safe")),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(root));
        assert!(parse_torrent(&raw).is_err());
    }

    #[test]
    fn reject_invalid_pieces_length() {
        // 19 bytes is not a multiple of 20
        let bad_pieces = vec![0u8; 19];
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(1024)),
            (b"name", BValue::Bytes(b"bad.bin")),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&bad_pieces)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(root));
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidPiecesLength(19))
        ));
    }

    #[test]
    fn reject_zero_piece_length() {
        let pieces_data = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(1024)),
            (b"name", BValue::Bytes(b"bad.bin")),
            (b"piece length", BValue::Int(0)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(root));
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidPieceLength(0))
        ));
    }

    #[test]
    fn zero_length_file_accepted() {
        let raw = multi_file_torrent("dir", &[("empty.txt", 0), ("data.bin", 100)]);
        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert_eq!(m.files[0].length, 0);
        assert_eq!(m.files[1].offset, 0); // empty file doesn't advance offset
    }

    fn v2_torrent(name: &str, file_name: &str, length: i64) -> Vec<u8> {
        let pieces_root = vec![0xABu8; 32];
        let leaf = BValue::Dict({
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"length", BValue::Int(length)),
                (b"pieces root", BValue::Bytes(&pieces_root)),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        });
        let file_node = BValue::Dict(vec![(b"".as_ref(), leaf)]);
        let file_tree = BValue::Dict(vec![(file_name.as_bytes(), file_node)]);

        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"file tree", file_tree),
            (b"meta version", BValue::Int(2)),
            (b"name", BValue::Bytes(name.as_bytes())),
            (b"piece length", BValue::Int(16 * 1024)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));

        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        encode(&BValue::Dict(root))
    }

    #[test]
    fn parse_v2_torrent() {
        let raw = v2_torrent("mydir", "data.bin", 65536);
        let meta = parse_torrent(&raw).unwrap();
        let TorrentMeta::V2(m) = meta else {
            panic!("expected V2")
        };
        assert_eq!(m.name, "mydir");
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].length, 65536);
        assert_eq!(m.info_hash_v2.len(), 32);
    }

    #[test]
    fn v2_infohash_uses_sha256() {
        let raw = v2_torrent("x", "f", 1024);
        let TorrentMeta::V2(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V2")
        };
        // SHA-256 produces 32-byte output; verify it's non-zero
        assert_ne!(m.info_hash_v2, [0u8; 32]);
    }

    #[test]
    fn torrent_meta_helpers() {
        let raw_v1 = single_file_torrent("t.bin", 512, 512 * 1024, None);
        let meta_v1 = parse_torrent(&raw_v1).unwrap();
        assert!(meta_v1.v1_info_hash().is_some());
        assert!(meta_v1.v2_info_hash().is_none());
        assert_eq!(meta_v1.name(), "t.bin");

        let raw_v2 = v2_torrent("mydir", "f", 1024);
        let meta_v2 = parse_torrent(&raw_v2).unwrap();
        assert!(meta_v2.v1_info_hash().is_none());
        assert!(meta_v2.v2_info_hash().is_some());
    }
}
