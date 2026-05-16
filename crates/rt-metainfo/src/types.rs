use rt_path::SafeRelPath;

/// Top-level torrent identity enum — v2 and hybrid are scaffolded, implemented in Phase 11.
#[derive(Debug, Clone)]
pub enum TorrentMeta {
    V1(TorrentMetaV1),
}

/// Parsed v1 torrent metainfo.
#[derive(Debug, Clone)]
pub struct TorrentMetaV1 {
    /// SHA-1 of the exact bencoded `info` dictionary bytes.
    pub info_hash: [u8; 20],
    pub announce: Option<String>,
    /// BEP 12 multi-tracker tiers.
    pub announce_list: Vec<Vec<String>>,
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
}
