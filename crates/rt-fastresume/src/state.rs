use std::{collections::HashMap, marker::PhantomData};

use serde::{
    de::{self, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};

pub const FASTRESUME_VERSION: u32 = 1;
pub const MAX_FASTRESUME_PIECES: usize = 16_000_000;
pub const MAX_FASTRESUME_PARTIAL_PIECES: usize = 16_384;
pub const MAX_FASTRESUME_BLOCKS_PER_PARTIAL_PIECE: usize = 16_384;
pub const MAX_FASTRESUME_FILE_HINTS: usize = 100_000;
pub const MAX_FASTRESUME_DIRTY_PIECES: usize = MAX_FASTRESUME_PIECES;

fn deserialize_bounded_vec<'de, D, T>(
    deserializer: D,
    maximum: usize,
    field: &'static str,
) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct BoundedVecVisitor<T> {
        maximum: usize,
        field: &'static str,
        marker: PhantomData<fn() -> T>,
    }

    impl<'de, T> Visitor<'de> for BoundedVecVisitor<T>
    where
        T: Deserialize<'de>,
    {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                formatter,
                "a {} array with at most {} items",
                self.field, self.maximum
            )
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            if sequence.size_hint().is_some_and(|size| size > self.maximum) {
                return Err(de::Error::custom(format!(
                    "{} exceeds the maximum of {} items",
                    self.field, self.maximum
                )));
            }
            let mut values =
                Vec::with_capacity(sequence.size_hint().unwrap_or_default().min(self.maximum));
            while let Some(value) = sequence.next_element()? {
                if values.len() >= self.maximum {
                    return Err(de::Error::custom(format!(
                        "{} exceeds the maximum of {} items",
                        self.field, self.maximum
                    )));
                }
                values.push(value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_seq(BoundedVecVisitor {
        maximum,
        field,
        marker: PhantomData,
    })
}

fn deserialize_piece_states<'de, D>(deserializer: D) -> Result<Vec<PieceState>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec(deserializer, MAX_FASTRESUME_PIECES, "fastresume pieces")
}

fn deserialize_partial_pieces<'de, D>(deserializer: D) -> Result<Vec<PartialPieceState>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec(
        deserializer,
        MAX_FASTRESUME_PARTIAL_PIECES,
        "fastresume partial pieces",
    )
}

fn deserialize_file_hints<'de, D>(deserializer: D) -> Result<Vec<FileHint>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec(
        deserializer,
        MAX_FASTRESUME_FILE_HINTS,
        "fastresume file hints",
    )
}

fn deserialize_dirty_pieces<'de, D>(deserializer: D) -> Result<Vec<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec(
        deserializer,
        MAX_FASTRESUME_DIRTY_PIECES,
        "fastresume dirty pieces",
    )
}

fn deserialize_received_blocks<'de, D>(deserializer: D) -> Result<Vec<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec(
        deserializer,
        MAX_FASTRESUME_BLOCKS_PER_PARTIAL_PIECE,
        "fastresume partial-piece blocks",
    )
}

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
    #[serde(deserialize_with = "deserialize_received_blocks")]
    pub received_blocks: Vec<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DurabilityWatermark {
    /// Completed storage-sync barrier generation. Valid pieces at or below this
    /// generation can be trusted after a clean fastresume load.
    #[serde(default)]
    pub barrier_generation: u64,
    /// Pieces written or revalidated since the last completed barrier. If the
    /// process crashes before a barrier completes, only these pieces need to be
    /// downgraded for bounded recheck.
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_dirty_pieces")]
    pub dirty_pieces_since_barrier: Vec<u32>,
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
    /// Hex-encoded torrent infohash. V1 uses 20 bytes; BEP 52 v2 uses 32 bytes.
    pub info_hash: String,
    /// Generation counter — incremented on each clean save.
    pub session_generation: u64,
    /// Per-piece verification state. Index = piece index.
    #[serde(deserialize_with = "deserialize_piece_states")]
    pub pieces: Vec<PieceState>,
    /// Received-but-not-yet-verified block indexes for partial pieces.
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_partial_pieces")]
    pub partial_pieces: Vec<PartialPieceState>,
    /// Per-file hints for fast re-validation.
    #[serde(deserialize_with = "deserialize_file_hints")]
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
    /// Storage durability checkpoint state for bounded post-crash recheck.
    #[serde(default)]
    pub durability: DurabilityWatermark,
}

impl FastresumeState {
    pub fn new_empty(info_hash: &[u8], piece_count: u32, policy: ImportPolicy) -> Self {
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
            durability: DurabilityWatermark::default(),
        }
    }

    /// Validate that stored state is compatible with the current torrent.
    pub fn validate(
        &self,
        info_hash: &[u8],
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
        let ranges = piece_map
            .piece_ranges_for_file_indices(&[file_index])
            .unwrap_or_default();
        self.invalidate_piece_ranges(&ranges)
    }

    /// Update file hints. If a hint has changed, invalidate affected pieces.
    pub fn apply_file_hints(
        &mut self,
        new_hints: Vec<FileHint>,
        piece_map: &rt_piece_map::PieceMap,
    ) -> u32 {
        let mut old_by_file = HashMap::with_capacity(self.file_hints.len());
        for hint in &self.file_hints {
            old_by_file.entry(hint.file_index).or_insert(hint);
        }
        let mut new_by_file = HashMap::with_capacity(new_hints.len());
        for hint in &new_hints {
            new_by_file.entry(hint.file_index).or_insert(hint);
        }

        let mut file_indices = old_by_file
            .keys()
            .copied()
            .chain(new_by_file.keys().copied())
            .collect::<Vec<_>>();
        file_indices.sort_unstable();
        file_indices.dedup();

        let mut changed_file_indices = Vec::new();
        for file_index in file_indices {
            let old = old_by_file.get(&file_index).copied();
            let new = new_by_file.get(&file_index).copied();
            let changed = match (old, new) {
                (Some(old), Some(new)) => {
                    old.size != new.size
                        || old.mtime_secs != new.mtime_secs
                        || old.inode != new.inode
                }
                // Treat both newly visible and missing files as changed. The
                // latter matters when a previously hinted file was deleted:
                // no current hint exists to trigger the old one-sided check.
                _ => true,
            };

            if changed {
                changed_file_indices.push(file_index);
            }
        }
        let ranges = piece_map
            .piece_ranges_for_file_indices(&changed_file_indices)
            .unwrap_or_default();
        let ranges = merge_piece_ranges(ranges);
        let invalidated = self.invalidate_piece_ranges(&ranges);
        self.file_hints = new_hints;
        invalidated
    }

    fn invalidate_piece_ranges(&mut self, ranges: &[(u32, u32)]) -> u32 {
        let mut count = 0u32;
        for &(first, last) in ranges {
            for piece in first..last {
                if self.pieces.get_mut(piece as usize).is_some_and(|state| {
                    if *state == PieceState::Valid {
                        *state = PieceState::Unknown;
                        true
                    } else {
                        false
                    }
                }) {
                    count = count.saturating_add(1);
                }
            }
        }

        // A partial piece is also tied to the bytes in a changed file. Keeping
        // its block indexes after a replacement or disappearance would make
        // the next run trust bytes that were never verified against the new
        // file. The ranges are sorted and merged, so membership is logarithmic.
        self.partial_pieces
            .retain(|partial| !piece_in_ranges(ranges, partial.piece));
        count
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

    pub fn set_dirty_pieces_since_barrier<I>(&mut self, pieces: I)
    where
        I: IntoIterator<Item = u32>,
    {
        let mut pieces = pieces.into_iter().collect::<Vec<_>>();
        pieces.sort_unstable();
        pieces.dedup();
        pieces.retain(|piece| (*piece as usize) < self.pieces.len());
        self.durability.dirty_pieces_since_barrier = pieces;
    }

    pub fn complete_durability_barrier(&mut self) {
        self.durability.barrier_generation = self.durability.barrier_generation.saturating_add(1);
        self.durability.dirty_pieces_since_barrier.clear();
        self.clean_shutdown = true;
    }

    pub fn apply_unclean_shutdown_watermark(&mut self) -> Option<u32> {
        if self.clean_shutdown {
            return Some(0);
        }
        if self.durability.dirty_pieces_since_barrier.is_empty() {
            return None;
        }
        // Fastresume input is bounded but not necessarily canonical. Sort and
        // deduplicate once so filtering partial records below remains
        // logarithmic instead of doing a full dirty-list scan per record.
        self.durability.dirty_pieces_since_barrier.sort_unstable();
        self.durability.dirty_pieces_since_barrier.dedup();
        let mut downgraded = 0;
        for piece in self.durability.dirty_pieces_since_barrier.iter().copied() {
            let Some(state) = self.pieces.get_mut(piece as usize) else {
                continue;
            };
            if *state == PieceState::Valid {
                *state = PieceState::Unknown;
                downgraded += 1;
            }
        }
        self.partial_pieces.retain(|partial| {
            !self
                .durability
                .dirty_pieces_since_barrier
                .binary_search(&partial.piece)
                .is_ok()
        });
        self.clean_shutdown = true;
        self.durability.dirty_pieces_since_barrier.clear();
        Some(downgraded)
    }
}

fn merge_piece_ranges(mut ranges: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    ranges.sort_unstable();
    let mut merged = Vec::with_capacity(ranges.len());
    for (first, last) in ranges {
        if let Some((_, current_last)) = merged.last_mut() {
            if first <= *current_last {
                *current_last = (*current_last).max(last);
                continue;
            }
        }
        merged.push((first, last));
    }
    merged
}

fn piece_in_ranges(ranges: &[(u32, u32)], piece: u32) -> bool {
    ranges
        .binary_search_by(|(first, last)| {
            if piece < *first {
                std::cmp::Ordering::Greater
            } else if piece >= *last {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
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

    fn test_hash_v2() -> [u8; 32] {
        [2u8; 32]
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
    fn validate_succeeds_for_v2_infohash() {
        let state =
            FastresumeState::new_empty(&test_hash_v2(), 8, ImportPolicy::RequireVerification);
        assert_eq!(state.info_hash, hex::encode(test_hash_v2()));
        assert!(state.validate(&test_hash_v2(), 8).is_ok());
        assert!(matches!(
            state.validate(&test_hash(), 8),
            Err(crate::error::FastresumeError::InfohashMismatch { .. })
        ));
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
    fn deserialization_rejects_oversized_partial_piece_block_arrays() {
        let partial = PartialPieceState {
            piece: 0,
            received_blocks: (0..=MAX_FASTRESUME_BLOCKS_PER_PARTIAL_PIECE as u32).collect(),
        };
        let encoded = serde_json::to_vec(&partial).unwrap();

        assert!(serde_json::from_slice::<PartialPieceState>(&encoded).is_err());
    }

    #[test]
    fn deserialization_rejects_oversized_partial_piece_arrays() {
        let mut state = FastresumeState::new_empty(&test_hash(), 1, ImportPolicy::TrustHints);
        state.partial_pieces = (0..=MAX_FASTRESUME_PARTIAL_PIECES)
            .map(|piece| PartialPieceState {
                piece: u32::try_from(piece).unwrap(),
                received_blocks: vec![0],
            })
            .collect();
        let encoded = serde_json::to_vec(&state).unwrap();

        assert!(serde_json::from_slice::<FastresumeState>(&encoded).is_err());
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
    fn file_replacement_and_disappearance_invalidate_partial_state() {
        let pm = make_pm(64, 256);
        let mut state = FastresumeState::new_empty(&test_hash(), 4, ImportPolicy::TrustHints);
        state.pieces.fill(PieceState::Valid);
        state.pieces[1] = PieceState::Unknown;
        state.partial_pieces = vec![PartialPieceState {
            piece: 1,
            received_blocks: vec![0],
        }];
        state.file_hints = vec![FileHint {
            file_index: 0,
            size: 256,
            mtime_secs: 1000,
            inode: 7,
        }];

        // A replacement with the same size and timestamp still has different
        // bytes when its inode changes.
        let invalidated = state.apply_file_hints(
            vec![FileHint {
                file_index: 0,
                size: 256,
                mtime_secs: 1000,
                inode: 8,
            }],
            &pm,
        );
        assert_eq!(invalidated, 3);
        assert!(state.pieces[..3]
            .iter()
            .all(|state| *state == PieceState::Unknown));
        assert!(state.partial_pieces.is_empty());

        state.pieces.fill(PieceState::Valid);
        state.file_hints = vec![FileHint {
            file_index: 0,
            size: 256,
            mtime_secs: 1000,
            inode: 8,
        }];
        state.partial_pieces = vec![PartialPieceState {
            piece: 2,
            received_blocks: vec![0],
        }];

        // A missing current hint must invalidate the old file as well.
        let invalidated = state.apply_file_hints(Vec::new(), &pm);
        assert_eq!(invalidated, 4);
        assert!(state
            .pieces
            .iter()
            .all(|state| *state == PieceState::Unknown));
        assert!(state.partial_pieces.is_empty());
    }

    #[test]
    fn missing_synthetic_padding_hint_does_not_invalidate_verified_pieces() {
        let files = vec![
            FileSpan {
                file_index: 0,
                path: SafeRelPath::from_name("payload.bin", false).unwrap(),
                content_offset: 0,
                length: 3,
            },
            FileSpan {
                file_index: 1,
                path: SafeRelPath::from_components(&[".pad", "13"], false).unwrap(),
                content_offset: 3,
                length: 13,
            },
        ];
        let piece_map = PieceMap::new_with_padding(16, files, [1]).unwrap();
        let mut state = FastresumeState::new_empty(&test_hash(), 1, ImportPolicy::TrustHints);
        state.pieces[0] = PieceState::Valid;
        state.file_hints = vec![FileHint {
            file_index: 1,
            size: 13,
            mtime_secs: 1,
            inode: 1,
        }];

        let invalidated = state.apply_file_hints(Vec::new(), &piece_map);

        assert_eq!(invalidated, 0);
        assert!(state.is_complete());
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

    #[test]
    fn durability_barrier_clears_dirty_piece_watermark() {
        let mut state =
            FastresumeState::new_empty(&test_hash(), 5, ImportPolicy::RequireVerification);
        state.clean_shutdown = false;
        state.set_dirty_pieces_since_barrier([3, 1, 3, 99]);
        assert_eq!(state.durability.dirty_pieces_since_barrier, vec![1, 3]);

        state.complete_durability_barrier();
        assert!(state.clean_shutdown);
        assert_eq!(state.durability.barrier_generation, 1);
        assert!(state.durability.dirty_pieces_since_barrier.is_empty());
    }

    #[test]
    fn unclean_watermark_downgrades_only_dirty_valid_pieces() {
        let mut state =
            FastresumeState::new_empty(&test_hash(), 5, ImportPolicy::RequireVerification);
        state.pieces = vec![
            PieceState::Valid,
            PieceState::Valid,
            PieceState::Valid,
            PieceState::Unknown,
            PieceState::Valid,
        ];
        state.partial_pieces = vec![
            PartialPieceState {
                piece: 1,
                received_blocks: vec![0],
            },
            PartialPieceState {
                piece: 4,
                received_blocks: vec![0],
            },
        ];
        state.set_dirty_pieces_since_barrier([1, 3]);

        assert_eq!(state.apply_unclean_shutdown_watermark(), Some(1));
        assert!(state.clean_shutdown);
        assert_eq!(
            state.pieces,
            vec![
                PieceState::Valid,
                PieceState::Unknown,
                PieceState::Valid,
                PieceState::Unknown,
                PieceState::Valid,
            ]
        );
        assert_eq!(state.partial_pieces.len(), 1);
        assert_eq!(state.partial_pieces[0].piece, 4);
    }

    #[test]
    fn unclean_without_watermark_requires_full_recheck() {
        let mut state =
            FastresumeState::new_empty(&test_hash(), 2, ImportPolicy::RequireVerification);
        state.pieces = vec![PieceState::Valid, PieceState::Valid];
        state.clean_shutdown = false;
        assert_eq!(state.apply_unclean_shutdown_watermark(), None);
    }

    #[test]
    fn unclean_watermark_canonicalizes_loaded_dirty_piece_order() {
        let mut state =
            FastresumeState::new_empty(&test_hash(), 5, ImportPolicy::RequireVerification);
        state.pieces.fill(PieceState::Valid);
        state.partial_pieces = vec![
            PartialPieceState {
                piece: 1,
                received_blocks: vec![0],
            },
            PartialPieceState {
                piece: 4,
                received_blocks: vec![0],
            },
        ];
        state.durability.dirty_pieces_since_barrier = vec![4, 1, 4, 99, 1];

        assert_eq!(state.apply_unclean_shutdown_watermark(), Some(2));
        assert_eq!(
            state.durability.dirty_pieces_since_barrier,
            Vec::<u32>::new()
        );
        assert!(state.partial_pieces.is_empty());
        assert_eq!(state.pieces[1], PieceState::Unknown);
        assert_eq!(state.pieces[4], PieceState::Unknown);
        assert!(state.pieces[0..1]
            .iter()
            .chain(state.pieces[2..4].iter())
            .all(|piece| *piece == PieceState::Valid));
    }
}
