use sha1::{Digest, Sha1};

use rt_bencode::{decode_torrent_info_span, BValue};
use rt_path::SafeRelPath;

use crate::{
    error::MetainfoError,
    types::{TorrentFileV1, TorrentMeta, TorrentMetaV1},
};

/// Parse a `.torrent` file from raw bytes.
pub fn parse_torrent(raw: &[u8]) -> Result<TorrentMeta, MetainfoError> {
    let (val, info_span) = decode_torrent_info_span(raw)?;

    let root = match &val {
        BValue::Dict(_) => &val,
        _ => return Err(MetainfoError::MissingField("root dict")),
    };

    let info = root
        .get(b"info")
        .ok_or(MetainfoError::MissingField("info"))?;

    // Compute infohash from the exact bytes of the info value
    let info_hash = if let Some(span) = info_span {
        let mut h = Sha1::new();
        h.update(&raw[span]);
        h.finalize().into()
    } else {
        return Err(MetainfoError::MissingField("info span"));
    };

    let name = get_string(info, b"name", "name")?;
    if name.is_empty() {
        return Err(MetainfoError::ZeroLengthName);
    }

    let piece_length = get_int(info, b"piece length", "piece length")? as u64;
    if piece_length == 0 || piece_length & (piece_length - 1) != 0 {
        return Err(MetainfoError::InvalidPieceLength(piece_length));
    }

    let pieces_bytes = get_bytes(info, b"pieces", "pieces")?;
    if pieces_bytes.len() % 20 != 0 {
        return Err(MetainfoError::InvalidPiecesLength(pieces_bytes.len()));
    }
    let pieces: Vec<[u8; 20]> = pieces_bytes
        .chunks_exact(20)
        .map(|c| c.try_into().unwrap())
        .collect();

    let private = info
        .get(b"private")
        .and_then(|v| v.as_int())
        .map(|i| i == 1)
        .unwrap_or(false);

    let files = parse_files(info, &name)?;

    let announce = root
        .get(b"announce")
        .and_then(|v| v.as_bytes())
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(|s| s.to_owned());

    let announce_list = parse_announce_list(root);

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

fn parse_files(info: &BValue<'_>, name: &str) -> Result<Vec<TorrentFileV1>, MetainfoError> {
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
        let TorrentMeta::V1(m) = meta;
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
        let TorrentMeta::V1(m) = meta;
        assert_eq!(m.files.len(), 2);
        assert_eq!(m.files[0].offset, 0);
        assert_eq!(m.files[1].offset, 100);
        assert_eq!(m.total_length(), 300);
        assert!(!m.is_single_file());
    }

    #[test]
    fn parse_private_flag() {
        let raw = single_file_torrent("priv.bin", 512, 512 * 1024, Some(1));
        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap();
        assert!(m.private);
    }

    #[test]
    fn infohash_stable_across_parses() {
        let raw = single_file_torrent("stable.bin", 1024, 512 * 1024, None);
        let TorrentMeta::V1(m1) = parse_torrent(&raw).unwrap();
        let TorrentMeta::V1(m2) = parse_torrent(&raw).unwrap();
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
        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap();
        assert_eq!(m.files[0].length, 0);
        assert_eq!(m.files[1].offset, 0); // empty file doesn't advance offset
    }
}
