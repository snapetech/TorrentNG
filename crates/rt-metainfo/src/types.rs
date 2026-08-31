use rt_path::SafeRelPath;

/// Top-level torrent identity. V2 and Hybrid are first-class BEP 52 metadata.
#[derive(Debug, Clone)]
pub enum TorrentMeta {
    V1(TorrentMetaV1),
    V2(TorrentMetaV2),
    /// Both v1 SHA-1 and v2 SHA-256 infohashes are valid; clients can use either.
    Hybrid(TorrentMetaV1, TorrentMetaV2),
}

impl TorrentMeta {
    /// v1 SHA-1 infohash (20 bytes), if present.
    pub fn v1_info_hash(&self) -> Option<[u8; 20]> {
        match self {
            TorrentMeta::V1(m) => Some(m.info_hash),
            TorrentMeta::Hybrid(m, _) => Some(m.info_hash),
            TorrentMeta::V2(_) => None,
        }
    }

    /// v2 SHA-256 infohash (32 bytes), if present.
    pub fn v2_info_hash(&self) -> Option<[u8; 32]> {
        match self {
            TorrentMeta::V2(m) => Some(m.info_hash_v2),
            TorrentMeta::Hybrid(_, m) => Some(m.info_hash_v2),
            TorrentMeta::V1(_) => None,
        }
    }

    /// The primary display name.
    pub fn name(&self) -> &str {
        match self {
            TorrentMeta::V1(m) => &m.name,
            TorrentMeta::V2(m) => &m.name,
            TorrentMeta::Hybrid(m, _) => &m.name,
        }
    }

    /// BEP 27 private flag.
    pub fn is_private(&self) -> bool {
        match self {
            TorrentMeta::V1(m) => m.private,
            TorrentMeta::V2(m) => m.private,
            TorrentMeta::Hybrid(m, _) => m.private,
        }
    }

    pub fn comment(&self) -> Option<&str> {
        match self {
            TorrentMeta::V1(m) => m.comment.as_deref(),
            TorrentMeta::V2(m) => m.comment.as_deref(),
            TorrentMeta::Hybrid(m, _) => m.comment.as_deref(),
        }
    }

    pub fn created_by(&self) -> Option<&str> {
        match self {
            TorrentMeta::V1(m) => m.created_by.as_deref(),
            TorrentMeta::V2(m) => m.created_by.as_deref(),
            TorrentMeta::Hybrid(m, _) => m.created_by.as_deref(),
        }
    }

    pub fn creation_date(&self) -> Option<i64> {
        match self {
            TorrentMeta::V1(m) => m.creation_date,
            TorrentMeta::V2(m) => m.creation_date,
            TorrentMeta::Hybrid(m, _) => m.creation_date,
        }
    }
}

/// Parsed v1 torrent metainfo.
#[derive(Debug, Clone)]
pub struct TorrentMetaV1 {
    /// SHA-1 of the exact bencoded `info` dictionary bytes.
    pub info_hash: [u8; 20],
    pub announce: Option<String>,
    /// BEP 12 multi-tracker tiers.
    pub announce_list: Vec<Vec<String>>,
    /// BEP 19 HTTP/FTP web seeds.
    pub webseeds: Vec<String>,
    pub comment: Option<String>,
    pub created_by: Option<String>,
    pub creation_date: Option<i64>,
    pub name: String,
    pub piece_length: u64,
    /// One SHA-1 hash per piece.
    pub pieces: Vec<[u8; 20]>,
    pub files: Vec<TorrentFileV1>,
    /// BEP 27 private flag.
    pub private: bool,
    /// Raw bytes of the entire .torrent, kept for re-announce and passthrough.
    pub raw: Vec<u8>,
}

impl TorrentMetaV1 {
    /// Total content length in bytes.
    pub fn total_length(&self) -> u64 {
        self.files.iter().map(|f| f.length).sum()
    }

    /// True if this is a single-file torrent.
    pub fn is_single_file(&self) -> bool {
        self.files.len() == 1
    }

    /// All tracker URLs across all tiers, deduplicated.
    pub fn all_trackers(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        if let Some(a) = &self.announce {
            if seen.insert(a.clone()) {
                out.push(a.clone());
            }
        }
        for tier in &self.announce_list {
            for url in tier {
                if seen.insert(url.clone()) {
                    out.push(url.clone());
                }
            }
        }
        out
    }
}

/// One file within a v1 torrent.
#[derive(Debug, Clone)]
pub struct TorrentFileV1 {
    pub index: u32,
    pub length: u64,
    pub path: SafeRelPath,
    /// Byte offset of this file within the concatenated content stream.
    pub offset: u64,
    /// BEP 47 padding file (`"attr"` contains `'p'`). Real clients never
    /// materialize these on disk; they exist purely to align the next real
    /// file to a piece boundary. Downstream code must not require this
    /// file to exist, and must not treat it as wanted content.
    pub pad: bool,
}

/// Parsed v2 torrent metainfo (BEP 52).
#[derive(Debug, Clone)]
pub struct TorrentMetaV2 {
    /// SHA-256 of the exact bencoded `info` dictionary bytes.
    pub info_hash_v2: [u8; 32],
    pub announce: Option<String>,
    pub announce_list: Vec<Vec<String>>,
    pub webseeds: Vec<String>,
    pub comment: Option<String>,
    pub created_by: Option<String>,
    pub creation_date: Option<i64>,
    pub name: String,
    /// Piece length must be a power of two, minimum 16 KiB.
    pub piece_length: u64,
    pub files: Vec<TorrentFileV2>,
    pub private: bool,
    pub raw: Vec<u8>,
}

/// Parsed magnet link metadata before BEP 9 metadata exchange completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagnetLink {
    pub info_hash_v1: Option<[u8; 20]>,
    pub info_hash_v2: Option<[u8; 32]>,
    pub display_name: Option<String>,
    pub trackers: Vec<String>,
}

impl TorrentMetaV2 {
    pub fn total_length(&self) -> u64 {
        self.files.iter().map(|f| f.length).sum()
    }

    pub fn all_trackers(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        if let Some(a) = &self.announce {
            if seen.insert(a.clone()) {
                out.push(a.clone());
            }
        }
        for tier in &self.announce_list {
            for url in tier {
                if seen.insert(url.clone()) {
                    out.push(url.clone());
                }
            }
        }
        out
    }
}

/// One file within a v2 torrent, with its merkle root hash.
#[derive(Debug, Clone)]
pub struct TorrentFileV2 {
    pub index: u32,
    pub length: u64,
    pub path: SafeRelPath,
    pub offset: u64,
    /// SHA-256 merkle root of this file's 16 KiB leaf hashes.
    pub pieces_root: [u8; 32],
    /// BEP 47 padding file (`"attr"` contains `'p'`). See
    /// [`TorrentFileV1::pad`].
    pub pad: bool,
}
