use serde::{Deserialize, Serialize};

pub const FASTRESUME_VERSION: u32 = 1;

/// What we know about a single piece.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PieceState {
    /// Hash-verified as valid.
    Valid,
    /// Verified as invalid (corrupt or incomplete).
    Invalid,
    /// Not yet verified — must verify before serving.
    Unknown,
    /// File(s) composing this piece are missing from disk.
    Missing,
}

/// Per-file hints used to detect whether re-verification is needed.
///
/// These are optimistic hints: if they match, we trust the piece state.
/// If any hint mismatches, affected pieces are reset to `Unknown`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileHint {
    pub file_index: u32,
    pub size: u64,
    /// Modification time as seconds since UNIX epoch.
    pub mtime_secs: u64,
    /// Inode number (platform-specific, 0 if unavailable).
    pub inode: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PartialPieceState {
    pub piece: u32,
    pub received_blocks: Vec<u32>,
}

/// Policy controlling when pieces can be marked Valid without explicit hash check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportPolicy {
    /// Every piece must be hash-verified before being marked Valid.
    RequireVerification,
    /// Trust file hints: if size/mtime/inode match, mark pieces Valid.
    TrustHints,
    /// Trust all pieces as Valid (used when importing from a trusted prior session).
    TrustAll,
}

/// Durable per-torrent fastresume state.
///
/// This is an optimization layer, not the source of truth.
/// If integrity cannot be established (version mismatch, infohash mismatch,
/// corrupted file), the caller must fall back to full re-verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FastresumeState {
    pub version: u32,
    /// Hex-encoded SHA-1 infohash.
    pub info_hash: String,
    /// Generation counter — incremented on each clean save.
    pub session_generation: u64,
    /// Per-piece verification state. Index = piece index.
    pub pieces: Vec<PieceState>,
    /// Received-but-not-yet-verified block indexes for partial pieces.
    #[serde(default)]
    pub partial_pieces: Vec<PartialPieceState>,
    /// Per-file hints for fast re-validation.
    pub file_hints: Vec<FileHint>,
    /// Unix timestamp of last full verification (0 = never).
    pub last_full_verify: u64,
    /// True if state was saved cleanly (not due to crash).
    pub clean_shutdown: bool,
    /// Upload/download accounting for tracker announces.
    pub uploaded_bytes: u64,
    pub downloaded_bytes: u64,
    /// Import policy used when this state was created.
    pub import_policy: ImportPolicy,
}

impl FastresumeState {
    pub fn new_empty(info_hash: &[u8; 20], piece_count: u32, policy: ImportPolicy) -> Self {
        FastresumeState {
            version: FASTRESUME_VERSION,
            info_hash: hex::encode(info_hash),
            session_generation: 1,
            pieces: vec![PieceState::Unknown; piece_count as usize],
            partial_pieces: Vec::new(),
            file_hints: Vec::new(),
            last_full_verify: 0,
            clean_shutdown: false,
            uploaded_bytes: 0,
            downloaded_bytes: 0,
            import_policy: policy,
        }
    }

    /// Validate that stored state is compatible with the current torrent.
    pub fn validate(
        &self,
        info_hash: &[u8; 20],
        piece_count: u32,
    ) -> Result<(), crate::error::FastresumeError> {
        use crate::error::FastresumeError;

        if self.version != FASTRESUME_VERSION {
            return Err(FastresumeError::VersionMismatch {
                expected: FASTRESUME_VERSION,
                found: self.version,
            });
        }
        let expected_hash = hex::encode(info_hash);
        if self.info_hash != expected_hash {
            return Err(FastresumeError::InfohashMismatch {
                stored: self.info_hash.clone(),
                current: expected_hash,
            });
        }
        if self.pieces.len() as u32 != piece_count {
            return Err(FastresumeError::PieceCountMismatch {
                stored: self.pieces.len() as u32,
                current: piece_count,
            });
        }
        Ok(())
    }

    /// Invalidate pieces that overlap with a file whose hints have changed.
    ///
    /// Returns the count of pieces reset to Unknown.
    pub fn invalidate_file(&mut self, file_index: u32, piece_map: &rt_piece_map::PieceMap) -> u32 {
        let mut count = 0u32;
        for piece in 0..piece_map.piece_count {
            let regions = match piece_map.piece_to_file_regions(piece) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if regions.iter().any(|r| r.file_index == file_index)
                && self.pieces[piece as usize] == PieceState::Valid
            {
                self.pieces[piece as usize] = PieceState::Unknown;
                count += 1;
            }
        }
        count
    }

    /// Update file hints. If a hint has changed, invalidate affected pieces.
    pub fn apply_file_hints(
        &mut self,
        new_hints: Vec<FileHint>,
        piece_map: &rt_piece_map::PieceMap,
    ) -> u32 {
        let mut invalidated = 0u32;
        for new_hint in &new_hints {
            let changed = self
                .file_hints
                .iter()
                .find(|h| h.file_index == new_hint.file_index)
                .map(|old| old.size != new_hint.size || old.mtime_secs != new_hint.mtime_secs)
                .unwrap_or(true); // treat new files as changed

            if changed {
                invalidated += self.invalidate_file(new_hint.file_index, piece_map);
            }
        }
        self.file_hints = new_hints;
        invalidated
    }

    pub fn piece_count(&self) -> u32 {
        self.pieces.len() as u32
    }

    pub fn valid_piece_count(&self) -> u32 {
        self.pieces
            .iter()
            .filter(|&&p| p == PieceState::Valid)
            .count() as u32
    }

    pub fn unknown_piece_count(&self) -> u32 {
        self.pieces
            .iter()
            .filter(|&&p| p == PieceState::Unknown)
            .count() as u32
    }

    pub fn is_complete(&self) -> bool {
        self.pieces.iter().all(|&p| p == PieceState::Valid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rt_path::SafeRelPath;
    use rt_piece_map::{FileSpan, PieceMap};

    fn make_pm(piece_len: u64, total: u64) -> PieceMap {
        let files = vec![FileSpan {
            file_index: 0,
            path: SafeRelPath::from_name("a.bin", false).unwrap(),
            content_offset: 0,
            length: total,
        }];
        PieceMap::new(piece_len, files).unwrap()
    }

    fn test_hash() -> [u8; 20] {
        [1u8; 20]
    }

    #[test]
    fn new_state_all_unknown() {
        let state = FastresumeState::new_empty(&test_hash(), 4, ImportPolicy::RequireVerification);
        assert_eq!(state.piece_count(), 4);
        assert_eq!(state.valid_piece_count(), 0);
        assert_eq!(state.unknown_piece_count(), 4);
        assert!(state.partial_pieces.is_empty());
        assert!(!state.is_complete());
    }

    #[test]
    fn validate_succeeds_for_matching_state() {
        let state = FastresumeState::new_empty(&test_hash(), 4, ImportPolicy::RequireVerification);
        assert!(state.validate(&test_hash(), 4).is_ok());
    }

    #[test]
    fn validate_rejects_infohash_mismatch() {
        let state = FastresumeState::new_empty(&[1u8; 20], 4, ImportPolicy::RequireVerification);
        let result = state.validate(&[2u8; 20], 4);
        assert!(matches!(
            result,
            Err(crate::error::FastresumeError::InfohashMismatch { .. })
        ));
    }

    #[test]
    fn validate_rejects_piece_count_mismatch() {
        let state = FastresumeState::new_empty(&test_hash(), 4, ImportPolicy::RequireVerification);
        let result = state.validate(&test_hash(), 5);
        assert!(matches!(
            result,
            Err(crate::error::FastresumeError::PieceCountMismatch { .. })
        ));
    }

    #[test]
    fn file_hint_change_invalidates_pieces() {
        let pm = make_pm(64, 256); // 4 pieces, 1 file
        let mut state = FastresumeState::new_empty(&test_hash(), 4, ImportPolicy::TrustHints);
        // Mark all valid
        for p in state.pieces.iter_mut() {
            *p = PieceState::Valid;
        }
        assert!(state.is_complete());

        let old_hints = vec![FileHint {
            file_index: 0,
            size: 256,
            mtime_secs: 1000,
            inode: 1,
        }];
        let new_hints_same = vec![FileHint {
            file_index: 0,
            size: 256,
            mtime_secs: 1000,
            inode: 1,
        }];
        let new_hints_changed = vec![FileHint {
            file_index: 0,
            size: 256,
            mtime_secs: 2000,
            inode: 1,
        }];

        // Apply same hints — nothing should be invalidated
        state.file_hints = old_hints.clone();
        let inv = state.apply_file_hints(new_hints_same, &pm);
        assert_eq!(inv, 0);
        assert!(state.is_complete());

        // Apply changed mtime — all pieces should be invalidated
        let inv2 = state.apply_file_hints(new_hints_changed, &pm);
        assert_eq!(inv2, 4);
        assert!(!state.is_complete());
        assert!(state.pieces.iter().all(|&p| p == PieceState::Unknown));
    }

    #[test]
    fn is_complete_only_when_all_valid() {
        let mut state =
            FastresumeState::new_empty(&test_hash(), 3, ImportPolicy::RequireVerification);
        state.pieces[0] = PieceState::Valid;
        state.pieces[1] = PieceState::Valid;
        assert!(!state.is_complete());
        state.pieces[2] = PieceState::Valid;
        assert!(state.is_complete());
    }
}
