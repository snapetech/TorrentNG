use rt_path::SafeRelPath;

use crate::{map::MAX_BLOCK_SIZE, PieceMapError};

/// A v2 file span in logical piece space. Unlike v1, non-empty files begin at
/// piece boundaries and therefore do not share a piece with another file.
#[derive(Debug, Clone)]
pub struct V2FileSpan {
    pub file_index: u32,
    pub path: SafeRelPath,
    pub piece_offset: u64,
    pub length: u64,
    pub pad: bool,
}

/// The file region occupied by one v2 peer-wire piece.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2PieceRegion {
    pub file_index: u32,
    pub path: SafeRelPath,
    pub file_offset: u64,
    pub length: u32,
    pub pad: bool,
}

/// Maps BEP 52 piece indexes to their owning files and validates block
/// requests. Alignment gaps are deliberately not exposed as file data.
#[derive(Debug, Clone)]
pub struct V2PieceMap {
    pub piece_length: u64,
    pub logical_length: u64,
    pub piece_count: u32,
    files: Vec<V2FileSpan>,
}

impl V2PieceMap {
    pub fn new(piece_length: u64, mut files: Vec<V2FileSpan>) -> Result<Self, PieceMapError> {
        if piece_length == 0 {
            return Err(PieceMapError::ZeroPieceLength);
        }
        if piece_length > u64::from(u32::MAX) {
            return Err(PieceMapError::PieceLengthTooLarge(piece_length));
        }
        files.sort_by_key(|file| file.piece_offset);
        // Empty files do not occupy v2 piece space. Keeping them in this
        // lookup table lets a later empty file at the same aligned offset
        // shadow the real file that owns the piece.
        files.retain(|file| file.length > 0);
        let mut logical_length = 0u64;
        let mut previous_end = 0u64;
        for file in &files {
            if file.length == 0 {
                continue;
            }
            if !file.piece_offset.is_multiple_of(piece_length) {
                return Err(PieceMapError::V2FileNotAligned {
                    file_index: file.file_index,
                    piece_length,
                });
            }
            let end = file
                .piece_offset
                .checked_add(file.length)
                .ok_or(PieceMapError::IntegerOverflow("v2 file span end"))?;
            if file.piece_offset < previous_end {
                return Err(PieceMapError::V2FileOverlap {
                    file_index: file.file_index,
                });
            }
            previous_end = end;
            logical_length = logical_length.max(end);
        }
        let piece_count_u64 = logical_length.div_ceil(piece_length);
        let piece_count = u32::try_from(piece_count_u64)
            .map_err(|_| PieceMapError::PieceCountTooLarge(piece_count_u64))?;
        Ok(Self {
            piece_length,
            logical_length,
            piece_count,
            files,
        })
    }

    /// Return the file-backed region for a piece. A missing region means the
    /// piece is only an alignment gap (or the metadata has no content).
    pub fn piece_to_file(&self, piece: u32) -> Result<V2PieceRegion, PieceMapError> {
        self.check_piece(piece)?;
        let piece_start = u64::from(piece)
            .checked_mul(self.piece_length)
            .ok_or(PieceMapError::IntegerOverflow("v2 piece start"))?;
        let candidate = self
            .files
            .partition_point(|file| file.piece_offset <= piece_start)
            .checked_sub(1);
        let Some(index) = candidate else {
            return Err(PieceMapError::NoV2FileForPiece(piece));
        };
        let file = &self.files[index];
        let file_end = file
            .piece_offset
            .checked_add(file.length)
            .ok_or(PieceMapError::IntegerOverflow("v2 file span end"))?;
        if file.length == 0 || piece_start >= file_end {
            return Err(PieceMapError::NoV2FileForPiece(piece));
        }
        let file_offset = piece_start - file.piece_offset;
        let length = (file_end - piece_start).min(self.piece_length);
        Ok(V2PieceRegion {
            file_index: file.file_index,
            path: file.path.clone(),
            file_offset,
            length: u32::try_from(length)
                .map_err(|_| PieceMapError::IntegerOverflow("v2 piece length"))?,
            pad: file.pad,
        })
    }

    pub fn piece_len(&self, piece: u32) -> Result<u32, PieceMapError> {
        Ok(self.piece_to_file(piece)?.length)
    }

    /// Validate a BEP 3 block request within a v2 piece.
    pub fn validate_request(
        &self,
        piece: u32,
        begin: u32,
        length: u32,
    ) -> Result<V2PieceRegion, PieceMapError> {
        if length == 0 {
            return Err(PieceMapError::ZeroRequestLength);
        }
        if length > MAX_BLOCK_SIZE {
            return Err(PieceMapError::BlockTooLarge(length, MAX_BLOCK_SIZE));
        }
        let region = self.piece_to_file(piece)?;
        if u64::from(begin) + u64::from(length) > u64::from(region.length) {
            return Err(PieceMapError::RequestOutOfBounds {
                offset: begin,
                len: length,
                piece_len: region.length,
            });
        }
        Ok(V2PieceRegion {
            file_offset: region
                .file_offset
                .checked_add(u64::from(begin))
                .ok_or(PieceMapError::IntegerOverflow("v2 request file offset"))?,
            length,
            ..region
        })
    }

    pub fn piece_ranges_for_file_indices(
        &self,
        file_indices: &[u32],
    ) -> Result<Vec<(u32, u32)>, PieceMapError> {
        let mut ranges = Vec::new();
        for file in &self.files {
            if file.length == 0 || file_indices.binary_search(&file.file_index).is_err() {
                continue;
            }
            let first = u32::try_from(file.piece_offset / self.piece_length)
                .map_err(|_| PieceMapError::IntegerOverflow("v2 file piece start"))?;
            let end = file
                .piece_offset
                .checked_add(file.length)
                .ok_or(PieceMapError::IntegerOverflow("v2 file span end"))?;
            let last = u32::try_from(end.div_ceil(self.piece_length))
                .map_err(|_| PieceMapError::IntegerOverflow("v2 file piece end"))?;
            if first < last {
                ranges.push((first, last));
            }
        }
        Ok(ranges)
    }

    fn check_piece(&self, piece: u32) -> Result<(), PieceMapError> {
        if piece >= self.piece_count {
            return Err(PieceMapError::PieceOutOfRange(piece, self.piece_count));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(index: u32, name: &str, piece_offset: u64, length: u64, pad: bool) -> V2FileSpan {
        V2FileSpan {
            file_index: index,
            path: SafeRelPath::from_components(&[name], false).unwrap(),
            piece_offset,
            length,
            pad,
        }
    }

    #[test]
    fn aligned_files_have_independent_piece_regions() {
        let map = V2PieceMap::new(
            16 * 1024,
            vec![
                span(0, "a", 0, 1000, false),
                span(1, "b", 16 * 1024, 16 * 1024 + 2, false),
            ],
        )
        .unwrap();
        assert_eq!(map.piece_count, 3);
        assert_eq!(map.piece_len(0).unwrap(), 1000);
        assert_eq!(map.piece_to_file(1).unwrap().file_index, 1);
        assert_eq!(map.piece_len(2).unwrap(), 2);
        assert_eq!(
            map.validate_request(2, 0, 2).unwrap().file_offset,
            16 * 1024
        );
    }

    #[test]
    fn alignment_gap_is_not_file_data() {
        let map = V2PieceMap::new(
            16 * 1024,
            vec![
                span(0, "a", 0, 16 * 1024 + 1, false),
                span(1, "b", 3 * 16 * 1024, 4, false),
            ],
        )
        .unwrap();
        assert!(matches!(map.piece_to_file(1), Ok(region) if region.file_index == 0));
        assert!(matches!(
            map.piece_to_file(2),
            Err(PieceMapError::NoV2FileForPiece(2))
        ));
        assert!(matches!(map.piece_to_file(3), Ok(region) if region.file_index == 1));
    }

    #[test]
    fn empty_file_does_not_shadow_real_piece() {
        let map = V2PieceMap::new(
            16 * 1024,
            vec![
                span(0, "data", 0, 1000, false),
                span(1, "empty", 0, 0, false),
            ],
        )
        .unwrap();
        assert!(matches!(
            map.piece_to_file(0),
            Ok(region) if region.file_index == 0
        ));
    }

    #[test]
    fn rejects_unaligned_file() {
        assert!(matches!(
            V2PieceMap::new(16 * 1024, vec![span(0, "a", 1, 1, false)]),
            Err(PieceMapError::V2FileNotAligned { .. })
        ));
    }
}
