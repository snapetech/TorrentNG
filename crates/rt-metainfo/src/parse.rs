use sha1::{Digest, Sha1};
use sha2::Sha256;

use rt_bencode::{decode_torrent_info_span, BValue};
use rt_path::SafeRelPath;

use crate::{
    error::MetainfoError,
    types::{TorrentFileV1, TorrentFileV2, TorrentMeta, TorrentMetaV1, TorrentMetaV2},
};

const MAX_TORRENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_FILES: usize = 100_000;
const MAX_PATH_COMPONENTS: usize = 256;
const MAX_TRACKER_URLS: usize = 4096;
const MAX_WEBSEED_URLS: usize = 4096;
const MAX_PIECES: usize = 16_000_000;

/// Parse a `.torrent` file from raw bytes. Handles v1, v2 (BEP 52), and hybrid.
pub fn parse_torrent(raw: &[u8]) -> Result<TorrentMeta, MetainfoError> {
    if raw.len() > MAX_TORRENT_BYTES {
        return Err(MetainfoError::LimitExceeded {
            field: "torrent bytes",
            limit: MAX_TORRENT_BYTES,
        });
    }

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
    let announce_list = parse_announce_list(root)?;
    let webseeds = parse_webseeds(root)?;
    let comment = parse_optional_string(root, b"comment");
    let created_by = parse_optional_string(root, b"created by");
    let creation_date = root.get(b"creation date").and_then(|v| v.as_int());

    let name = get_string(info, b"name", "name")?;
    if name.is_empty() {
        return Err(MetainfoError::ZeroLengthName);
    }

    let piece_length = get_positive_u64(info, b"piece length", "piece length")?;

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

        let pieces = parse_piece_hashes(info)?;
        let files_v1 = parse_files_v1(info, &name)?;
        let files_v2 = parse_file_tree(info, &name)?;

        return Ok(TorrentMeta::Hybrid(
            TorrentMetaV1 {
                info_hash: info_hash_v1,
                announce: announce.clone(),
                announce_list: announce_list.clone(),
                webseeds: webseeds.clone(),
                comment: comment.clone(),
                created_by: created_by.clone(),
                creation_date,
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
                webseeds,
                comment,
                created_by,
                creation_date,
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
            webseeds,
            comment,
            created_by,
            creation_date,
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
    let pieces = parse_piece_hashes(info)?;
    let files = parse_files_v1(info, &name)?;

    Ok(TorrentMeta::V1(TorrentMetaV1 {
        info_hash,
        announce,
        announce_list,
        webseeds,
        comment,
        created_by,
        creation_date,
        name,
        piece_length,
        pieces,
        files,
        private,
        raw: raw.to_vec(),
    }))
}

/// Return the exact bencoded `info` dictionary bytes used for v1 infohashes
/// and BEP 9 metadata exchange.
pub fn torrent_info_bytes(raw: &[u8]) -> Result<Vec<u8>, MetainfoError> {
    let (_, info_span) = decode_torrent_info_span(raw)?;
    let info_span = info_span.ok_or(MetainfoError::MissingField("info span"))?;
    Ok(raw[info_span].to_vec())
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

    // Unlike v1, BEP 52 does not special-case single-file torrents: the
    // file tree is always rooted under `name` as a container directory,
    // even when it holds exactly one file. Real v2-capable clients
    // (libtorrent-based: qBittorrent, rTorrent) place such a file at
    // `save_path/name/<leaf>`, not flatly at `save_path/name` - confirmed
    // against this crate's own cryptographically-verified fixtures in
    // `crates/rt-engine/src/engine.rs` (`pure_v2_recheck_verifies_file_roots_without_torrent_task`).
    // Do not "fix" this into flat placement without re-verifying against
    // real client output first.
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
    if out.len() >= MAX_FILES {
        return Err(MetainfoError::LimitExceeded {
            field: "files",
            limit: MAX_FILES,
        });
    }
    if path_components.len() > MAX_PATH_COMPONENTS {
        return Err(MetainfoError::LimitExceeded {
            field: "path components",
            limit: MAX_PATH_COMPONENTS,
        });
    }

    let dict = match node {
        BValue::Dict(pairs) => pairs,
        _ => return Err(MetainfoError::InvalidFieldType("file tree node")),
    };

    // Leaf: has empty-string key ""
    if let Some(leaf) = node.get(b"") {
        let length = get_nonnegative_u64(leaf, b"length", "file tree length")?;
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
            pad: is_pad_attr(leaf),
        });
        add_offset(offset, length, "file tree offset")?;
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
    parse_optional_string(root, b"announce")
}

fn parse_optional_string(root: &BValue<'_>, key: &[u8]) -> Option<String> {
    root.get(key)
        .and_then(|v| v.as_bytes())
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_files_v1(info: &BValue<'_>, name: &str) -> Result<Vec<TorrentFileV1>, MetainfoError> {
    if let Some(BValue::List(file_list)) = info.get(b"files") {
        if file_list.len() > MAX_FILES {
            return Err(MetainfoError::LimitExceeded {
                field: "files",
                limit: MAX_FILES,
            });
        }
        // Multi-file torrent: name is the root directory
        let mut offset = 0u64;
        let mut files = Vec::with_capacity(file_list.len());
        for (idx, entry) in file_list.iter().enumerate() {
            let length = get_nonnegative_u64(entry, b"length", "file length")?;
            let path_list = match entry.get(b"path") {
                Some(BValue::List(parts)) => parts,
                _ => return Err(MetainfoError::MissingField("file path")),
            };
            if path_list.len().saturating_add(1) > MAX_PATH_COMPONENTS {
                return Err(MetainfoError::LimitExceeded {
                    field: "path components",
                    limit: MAX_PATH_COMPONENTS,
                });
            }
            let mut components: Vec<String> = vec![name.to_owned()];
            for part in path_list {
                let s = match part {
                    BValue::Bytes(b) => std::str::from_utf8(b)
                        .map_err(|_| MetainfoError::InvalidUtf8("path component"))?
                        .to_owned(),
                    _ => return Err(MetainfoError::InvalidFieldType("path component")),
                };
                // Some (old, real-world) torrent creation tools emit a
                // vestigial empty leading path component - e.g.
                // `path: ["", "movie.mkv"]`. Confirmed against a real,
                // actively-seeding rTorrent production torrent: rTorrent
                // itself silently drops it (files land at
                // `<name>/movie.mkv`, no empty-named subdirectory), so
                // rejecting the whole file here would import less than a
                // real client does. `SafeRelPath` still rejects a path
                // that ends up with zero components after this filtering.
                if !s.is_empty() {
                    components.push(s);
                }
            }
            let path = SafeRelPath::from_components(&components, false)?;
            let pad = is_pad_attr(entry);
            files.push(TorrentFileV1 {
                index: idx as u32,
                length,
                path,
                offset,
                pad,
            });
            add_offset(&mut offset, length, "file offset")?;
        }
        Ok(files)
    } else {
        // Single-file torrent
        let length = get_nonnegative_u64(info, b"length", "length")?;
        let path = SafeRelPath::from_name(name, false)?;
        Ok(vec![TorrentFileV1 {
            index: 0,
            length,
            path,
            offset: 0,
            pad: false,
        }])
    }
}

fn parse_announce_list(root: &BValue<'_>) -> Result<Vec<Vec<String>>, MetainfoError> {
    let Some(BValue::List(tiers)) = root.get(b"announce-list") else {
        return Ok(Vec::new());
    };
    let mut total = 0usize;
    let mut out = Vec::new();
    for tier in tiers {
        let BValue::List(urls) = tier else {
            continue;
        };
        let mut tier_urls = Vec::new();
        for u in urls {
            let Some(url) = u
                .as_bytes()
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(str::trim)
                .filter(|url| !url.is_empty())
            else {
                continue;
            };
            total += 1;
            if total > MAX_TRACKER_URLS {
                return Err(MetainfoError::LimitExceeded {
                    field: "tracker urls",
                    limit: MAX_TRACKER_URLS,
                });
            }
            tier_urls.push(url.to_owned());
        }
        if !tier_urls.is_empty() {
            out.push(tier_urls);
        }
    }
    Ok(out)
}

fn parse_webseeds(root: &BValue<'_>) -> Result<Vec<String>, MetainfoError> {
    let Some(value) = root.get(b"url-list") else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    match value {
        BValue::Bytes(bytes) => push_webseed_bytes(bytes, &mut out)?,
        BValue::List(values) => {
            for value in values {
                if let Some(bytes) = value.as_bytes() {
                    push_webseed_bytes(bytes, &mut out)?;
                }
            }
        }
        _ => {}
    }
    Ok(out)
}

fn push_webseed_bytes(bytes: &[u8], out: &mut Vec<String>) -> Result<(), MetainfoError> {
    let Ok(value) = std::str::from_utf8(bytes) else {
        return Ok(());
    };
    let value = value.trim();
    if value.is_empty() || out.iter().any(|existing| existing == value) {
        return Ok(());
    }
    if out.len() >= MAX_WEBSEED_URLS {
        return Err(MetainfoError::LimitExceeded {
            field: "webseed urls",
            limit: MAX_WEBSEED_URLS,
        });
    }
    out.push(value.to_owned());
    Ok(())
}

fn parse_piece_hashes(info: &BValue<'_>) -> Result<Vec<[u8; 20]>, MetainfoError> {
    let pieces_bytes = get_bytes(info, b"pieces", "pieces")?;
    if pieces_bytes.len() % 20 != 0 {
        return Err(MetainfoError::InvalidPiecesLength(pieces_bytes.len()));
    }
    let piece_count = pieces_bytes.len() / 20;
    if piece_count > MAX_PIECES {
        return Err(MetainfoError::LimitExceeded {
            field: "pieces",
            limit: MAX_PIECES,
        });
    }
    Ok(pieces_bytes
        .chunks_exact(20)
        .map(|c| c.try_into().unwrap())
        .collect())
}

/// BEP 47: a file dict/leaf carries `"attr"` as a short string of one-letter
/// flags; `'p'` marks a padding file real clients never write to disk.
fn is_pad_attr(dict: &BValue<'_>) -> bool {
    match dict.get(b"attr") {
        Some(BValue::Bytes(b)) => b.contains(&b'p'),
        _ => false,
    }
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

fn get_nonnegative_u64(
    dict: &BValue<'_>,
    key: &[u8],
    field: &'static str,
) -> Result<u64, MetainfoError> {
    let value = get_int(dict, key, field)?;
    u64::try_from(value).map_err(|_| MetainfoError::InvalidIntegerValue { field, value })
}

/// BEP 3 recommends (does not require) a power-of-two piece length, and
/// nothing downstream (rt-piece-map, rt-piece-picker, rt-storage) relies on
/// it - all piece-boundary math here uses plain division/modulo, never bit
/// shifts. Requiring it anyway rejects real, actively-seeded torrents from
/// well-known release groups: confirmed against a 7351-torrent production
/// rTorrent session, where 43 legitimate torrents (e.g. a UHD BluRay remux
/// with `piece length` 7995392, not a power of two) were otherwise skipped
/// outright. Piece-count DoS protection is handled separately by
/// `MAX_PIECES` in `parse_piece_hashes`, independent of piece-length shape.
fn get_positive_u64(
    dict: &BValue<'_>,
    key: &[u8],
    field: &'static str,
) -> Result<u64, MetainfoError> {
    let value = get_nonnegative_u64(dict, key, field)?;
    if value == 0 {
        return Err(MetainfoError::InvalidPieceLength(value));
    }
    Ok(value)
}

fn add_offset(offset: &mut u64, length: u64, field: &'static str) -> Result<(), MetainfoError> {
    *offset = offset
        .checked_add(length)
        .ok_or(MetainfoError::IntegerOverflow(field))?;
    Ok(())
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
    fn parse_multi_file_marks_bep47_padding_files() {
        let real_pairs: Vec<(&[u8], BValue<'_>)> = {
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"length", BValue::Int(100)),
                (b"path", BValue::List(vec![BValue::Bytes(b"a.txt")])),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        };
        let pad_pairs: Vec<(&[u8], BValue<'_>)> = {
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"attr", BValue::Bytes(b"p")),
                (b"length", BValue::Int(28)),
                (
                    b"path",
                    BValue::List(vec![BValue::Bytes(b".pad"), BValue::Bytes(b"128")]),
                ),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        };
        let file_entries = vec![BValue::Dict(real_pairs), BValue::Dict(pad_pairs)];

        let pieces_data = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"files", BValue::List(file_entries)),
            (b"name", BValue::Bytes(b"mydir")),
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

        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert_eq!(m.files.len(), 2);
        assert!(!m.files[0].pad, "real file must not be marked pad");
        assert!(m.files[1].pad, "attr:p file must be marked pad");
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
    fn parse_top_level_comment_creator_and_creation_date() {
        let pieces_data = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(1024)),
            (b"name", BValue::Bytes(b"noted.bin")),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"comment", BValue::Bytes(b" Release notes ")),
            (b"created by", BValue::Bytes(b"TorrentNG test fixture")),
            (b"creation date", BValue::Int(1_700_000_000)),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(root));

        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert_eq!(m.comment.as_deref(), Some("Release notes"));
        assert_eq!(m.created_by.as_deref(), Some("TorrentNG test fixture"));
        assert_eq!(m.creation_date, Some(1_700_000_000));
    }

    #[test]
    fn parse_webseeds_accepts_string_and_list_forms() {
        let pieces_data = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(1024)),
            (b"name", BValue::Bytes(b"seeded.bin")),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
            (
                b"url-list",
                BValue::List(vec![
                    BValue::Bytes(b" https://seed.example/file "),
                    BValue::Bytes(b"https://seed.example/file"),
                    BValue::Bytes(b"https://mirror.example/file"),
                ]),
            ),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(root));
        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert_eq!(
            m.webseeds,
            vec![
                "https://seed.example/file".to_owned(),
                "https://mirror.example/file".to_owned()
            ]
        );
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
    fn accepts_non_power_of_two_piece_length() {
        // A real, currently-seeding torrent from a well-known release
        // group uses this exact non-power-of-two piece length; nothing
        // downstream requires a power of two (see get_positive_u64's doc
        // comment), and rejecting it means importing 100% of a real
        // library isn't actually achievable.
        let raw = single_file_torrent("Black.Phone.2.mkv", 1024, 7_995_392, None);
        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert_eq!(m.piece_length, 7_995_392);
    }

    #[test]
    fn multi_file_drops_vestigial_empty_leading_path_component() {
        // A real, ~20-year-old scene release, still actively seeding on a
        // real rTorrent box, encodes each file's path as
        // ["", "movie.mkv"] - a vestigial empty leading component. Real
        // clients (confirmed: rTorrent) place the file at
        // `<name>/movie.mkv` directly, no empty-named subdirectory.
        // Rejecting the whole torrent over this imports less than a real
        // client does.
        let file_entry = BValue::Dict({
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"length", BValue::Int(1024)),
                (
                    b"path",
                    BValue::List(vec![BValue::Bytes(b""), BValue::Bytes(b"movie.mkv")]),
                ),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        });
        let pieces_data = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"files", BValue::List(vec![file_entry])),
            (b"name", BValue::Bytes(b"Old.Release-GROUP")),
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

        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert_eq!(m.files.len(), 1);
        assert_eq!(
            m.files[0].path.as_display(),
            "Old.Release-GROUP/movie.mkv",
            "the empty component must be dropped, not preserved as a subdirectory"
        );
    }

    #[test]
    fn reject_negative_piece_length() {
        let raw = single_file_torrent("bad.bin", 1024, -1, None);
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidIntegerValue {
                field: "piece length",
                value: -1
            })
        ));
    }

    #[test]
    fn reject_i64_min_piece_length() {
        let raw = single_file_torrent("bad.bin", 1024, i64::MIN, None);
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidIntegerValue { field: "piece length", value }) if value == i64::MIN
        ));
    }

    #[test]
    fn reject_negative_single_file_length() {
        let raw = single_file_torrent("bad.bin", -1, 512 * 1024, None);
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidIntegerValue {
                field: "length",
                value: -1
            })
        ));
    }

    #[test]
    fn reject_negative_multi_file_length() {
        let raw = multi_file_torrent("dir", &[("bad.bin", -1)]);
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidIntegerValue {
                field: "file length",
                value: -1
            })
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

    // Regression coverage for the v2 single-file wrapper-directory bug:
    // real v2-capable clients (libtorrent-based qBittorrent/rTorrent,
    // Transmission) place a true single-file v2 torrent flatly at
    // `save_path/name`, matching v1 single-file placement. Only when the
    // tree genuinely encodes a subdirectory should `name/` be prepended.

    fn v2_multi_file_torrent(dir_name: &str, files: &[(&str, i64)]) -> Vec<u8> {
        let pieces_root = vec![0xCDu8; 32];
        let mut leaves: Vec<(&[u8], BValue<'_>)> = files
            .iter()
            .map(|(fname, length)| {
                let leaf = BValue::Dict({
                    let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                        (b"length", BValue::Int(*length)),
                        (b"pieces root", BValue::Bytes(&pieces_root)),
                    ];
                    p.sort_by(|a, b| a.0.cmp(b.0));
                    p
                });
                let node = BValue::Dict(vec![(b"".as_ref(), leaf)]);
                (fname.as_bytes(), node)
            })
            .collect();
        leaves.sort_by(|a, b| a.0.cmp(b.0));
        let file_tree = BValue::Dict(leaves);

        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"file tree", file_tree),
            (b"meta version", BValue::Int(2)),
            (b"name", BValue::Bytes(dir_name.as_bytes())),
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

    fn v2_single_file_in_subdir_torrent(
        name: &str,
        dir: &str,
        file_name: &str,
        length: i64,
    ) -> Vec<u8> {
        let pieces_root = vec![0xEFu8; 32];
        let leaf = BValue::Dict({
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"length", BValue::Int(length)),
                (b"pieces root", BValue::Bytes(&pieces_root)),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        });
        let file_node = BValue::Dict(vec![(b"".as_ref(), leaf)]);
        let subdir_node = BValue::Dict(vec![(file_name.as_bytes(), file_node)]);
        let file_tree = BValue::Dict(vec![(dir.as_bytes(), subdir_node)]);

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
    fn v2_single_file_keeps_name_as_wrapper_directory() {
        // Unlike v1, BEP 52 does not special-case single-file torrents:
        // even one file lives under `name/` as a container directory. Do
        // not "fix" this into flat `save_path/name` placement without
        // re-verifying against real client output first - an earlier
        // attempt at exactly that broke `crates/rt-engine/src/engine.rs`'s
        // cryptographically-verified v2 fixtures
        // (`pure_v2_recheck_verifies_file_roots_without_torrent_task`).
        let raw = v2_torrent("mydir", "data.bin", 65536);
        let TorrentMeta::V2(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V2")
        };
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].path.as_display(), "mydir/data.bin");
    }

    #[test]
    fn v2_leaf_marks_bep47_padding_files() {
        let real_root = vec![0x11u8; 32];
        let pad_root = vec![0x22u8; 32];
        let real_leaf = BValue::Dict({
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"length", BValue::Int(1000)),
                (b"pieces root", BValue::Bytes(&real_root)),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        });
        let pad_leaf = BValue::Dict({
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"attr", BValue::Bytes(b"p")),
                (b"length", BValue::Int(24)),
                (b"pieces root", BValue::Bytes(&pad_root)),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        });
        let file_tree = BValue::Dict({
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (
                    b"01.flac".as_ref(),
                    BValue::Dict(vec![(b"".as_ref(), real_leaf)]),
                ),
                (
                    b".pad".as_ref(),
                    BValue::Dict(vec![(
                        b"24".as_ref(),
                        BValue::Dict(vec![(b"".as_ref(), pad_leaf)]),
                    )]),
                ),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        });

        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"file tree", file_tree),
            (b"meta version", BValue::Int(2)),
            (b"name", BValue::Bytes(b"album")),
            (b"piece length", BValue::Int(16 * 1024)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(root));

        let TorrentMeta::V2(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V2")
        };
        assert_eq!(m.files.len(), 2);
        let real = m
            .files
            .iter()
            .find(|f| f.path.as_display() == "album/01.flac")
            .unwrap();
        let pad = m
            .files
            .iter()
            .find(|f| f.path.as_display() == "album/.pad/24")
            .unwrap();
        assert!(!real.pad);
        assert!(pad.pad);
    }

    #[test]
    fn v2_multi_file_root_keeps_name_as_wrapper_directory() {
        let raw = v2_multi_file_torrent("album", &[("01.flac", 1000), ("02.flac", 2000)]);
        let TorrentMeta::V2(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V2")
        };
        assert_eq!(m.files.len(), 2);
        let paths: Vec<String> = m.files.iter().map(|f| f.path.as_display()).collect();
        assert!(paths.contains(&"album/01.flac".to_owned()));
        assert!(paths.contains(&"album/02.flac".to_owned()));
    }

    #[test]
    fn v2_single_file_nested_in_subdirectory_keeps_name_and_subdir() {
        let raw = v2_single_file_in_subdir_torrent("mydir", "docs", "readme.txt", 42);
        let TorrentMeta::V2(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V2")
        };
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].path.as_display(), "mydir/docs/readme.txt");
    }

    #[test]
    fn reject_negative_v2_file_length() {
        let raw = v2_torrent("mydir", "bad.bin", -1);
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidIntegerValue {
                field: "file tree length",
                value: -1
            })
        ));
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
