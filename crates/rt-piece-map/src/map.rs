use rt_path::SafeRelPath;

use crate::error::PieceMapError;

/// BEP 3 maximum block size for request validation.
pub const MAX_BLOCK_SIZE: u32 = 16 * 1024;

/// A slice of a file that belongs to a piece.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRegion {
    pub file_index: u32,
    pub path: SafeRelPath,
    /// Byte offset within the file.
    pub file_offset: u64,
    /// Byte offset within the piece.
    pub piece_offset: u64,
    /// Length of this region.
    pub length: u64,
}

/// File spans in the order they appear in the torrent.
#[derive(Debug, Clone)]
pub struct FileSpan {
    pub file_index: u32,
    pub path: SafeRelPath,
    /// Byte offset of the file in the concatenated content stream.
    pub content_offset: u64,
    pub length: u64,
}

/// A slice of content belonging to a specific piece within a peer request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PieceRegion {
    pub file_index: u32,
    pub path: SafeRelPath,
    /// Offset within the file.
    pub file_offset: u64,
    /// How many bytes to read.
    pub length: u32,
}

/// Maps pieces to file regions and validates peer block requests.
#[derive(Debug, Clone)]
pub struct PieceMap {
    pub piece_length: u64,
    pub total_length: u64,
    pub piece_count: u32,
    files: Vec<FileSpan>,
}

impl PieceMap {
    /// Build a PieceMap from a list of files in content-stream order.
    pub fn new(piece_length: u64, files: Vec<FileSpan>) -> Result<Self, PieceMapError> {
        if piece_length == 0 {
            return Err(PieceMapError::ZeroPieceLength);
        }
        let total_length: u64 = files.iter().map(|f| f.length).sum();
        if total_length == 0 {
            return Err(PieceMapError::ZeroTotalLength);
        }
        let piece_count = total_length.div_ceil(piece_length) as u32;
        Ok(PieceMap {
            piece_length,
            total_length,
            piece_count,
            files,
        })
    }

    /// Byte length of a specific piece (last piece may be shorter).
    pub fn piece_len(&self, piece: u32) -> Result<u32, PieceMapError> {
        if piece >= self.piece_count {
            return Err(PieceMapError::PieceOutOfRange(piece, self.piece_count));
        }
        let start = piece as u64 * self.piece_length;
        let remaining = self.total_length - start;
        Ok(remaining.min(self.piece_length) as u32)
    }

    /// Byte range [start, end) within the content stream for a piece.
    pub fn piece_content_range(&self, piece: u32) -> Result<(u64, u64), PieceMapError> {
        if piece >= self.piece_count {
            return Err(PieceMapError::PieceOutOfRange(piece, self.piece_count));
        }
        let start = piece as u64 * self.piece_length;
        let end = (start + self.piece_length).min(self.total_length);
        Ok((start, end))
    }

    /// All file regions that a piece spans.
    pub fn piece_to_file_regions(&self, piece: u32) -> Result<Vec<FileRegion>, PieceMapError> {
        let (piece_start, piece_end) = self.piece_content_range(piece)?;
        let mut regions = Vec::new();
        for file in &self.files {
            let file_end = file.content_offset + file.length;
            let overlap_start = piece_start.max(file.content_offset);
            let overlap_end = piece_end.min(file_end);
            if overlap_start >= overlap_end {
                continue;
            }
            regions.push(FileRegion {
                file_index: file.file_index,
                path: file.path.clone(),
                file_offset: overlap_start - file.content_offset,
                piece_offset: overlap_start - piece_start,
                length: overlap_end - overlap_start,
            });
        }
        Ok(regions)
    }

    /// Validate a peer block request (BEP 3) and return file regions to read.
    ///
    /// Rejects requests where length > 16 KiB.
    pub fn validate_request(
        &self,
        piece: u32,
        begin: u32,
        length: u32,
    ) -> Result<Vec<PieceRegion>, PieceMapError> {
        if length > MAX_BLOCK_SIZE {
            return Err(PieceMapError::BlockTooLarge(length, MAX_BLOCK_SIZE));
        }
        let piece_len = self.piece_len(piece)?;
        if begin.saturating_add(length) > piece_len {
            return Err(PieceMapError::RequestOutOfBounds {
                offset: begin,
                len: length,
                piece_len,
            });
        }

        let piece_start = piece as u64 * self.piece_length;
        let request_start = piece_start + begin as u64;
        let request_end = request_start + length as u64;

        let mut regions = Vec::new();
        for file in &self.files {
            let file_end = file.content_offset + file.length;
            let overlap_start = request_start.max(file.content_offset);
            let overlap_end = request_end.min(file_end);
            if overlap_start >= overlap_end {
                continue;
            }
            regions.push(PieceRegion {
                file_index: file.file_index,
                path: file.path.clone(),
                file_offset: overlap_start - file.content_offset,
                length: (overlap_end - overlap_start) as u32,
            });
        }
        Ok(regions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_files(specs: &[(&[&str], u64)]) -> Vec<FileSpan> {
        let mut offset = 0u64;
        specs
            .iter()
            .enumerate()
            .map(|(i, (components, len))| {
                let span = FileSpan {
                    file_index: i as u32,
                    path: SafeRelPath::from_components(components, false).unwrap(),
                    content_offset: offset,
                    length: *len,
                };
                offset += len;
                span
            })
            .collect()
    }

    #[test]
    fn single_file_piece_count() {
        let files = make_files(&[(&["a.bin"], 1024 * 1024)]);
        let pm = PieceMap::new(256 * 1024, files).unwrap();
        assert_eq!(pm.piece_count, 4);
    }

    #[test]
    fn last_piece_shorter() {
        let files = make_files(&[(&["a.bin"], 300)]);
        let pm = PieceMap::new(256, files).unwrap();
        assert_eq!(pm.piece_count, 2);
        assert_eq!(pm.piece_len(0).unwrap(), 256);
        assert_eq!(pm.piece_len(1).unwrap(), 44);
    }

    #[test]
    fn single_file_region() {
        let files = make_files(&[(&["a.bin"], 1024)]);
        let pm = PieceMap::new(512, files).unwrap();
        let regions = pm.piece_to_file_regions(0).unwrap();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].file_offset, 0);
        assert_eq!(regions[0].length, 512);
    }

    #[test]
    fn piece_spanning_two_files() {
        // piece_length=512, file A=300 bytes, file B=300 bytes
        // piece 0: bytes 0..512 → A[0..300] + B[0..212]
        let files = make_files(&[(&["a.bin"], 300), (&["b.bin"], 300)]);
        let pm = PieceMap::new(512, files).unwrap();
        let regions = pm.piece_to_file_regions(0).unwrap();
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].file_index, 0);
        assert_eq!(regions[0].file_offset, 0);
        assert_eq!(regions[0].length, 300);
        assert_eq!(regions[1].file_index, 1);
        assert_eq!(regions[1].file_offset, 0);
        assert_eq!(regions[1].length, 212);
    }

    #[test]
    fn zero_length_file_skipped() {
        let files = make_files(&[(&["empty.txt"], 0), (&["data.bin"], 512)]);
        let pm = PieceMap::new(512, files).unwrap();
        let regions = pm.piece_to_file_regions(0).unwrap();
        // empty file contributes no bytes
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].file_index, 1);
    }

    #[test]
    fn piece_out_of_range() {
        let files = make_files(&[(&["a.bin"], 512)]);
        let pm = PieceMap::new(512, files).unwrap();
        assert!(matches!(
            pm.piece_len(1),
            Err(PieceMapError::PieceOutOfRange(1, 1))
        ));
    }

    #[test]
    fn request_too_large() {
        let files = make_files(&[(&["a.bin"], 1024 * 1024)]);
        let pm = PieceMap::new(256 * 1024, files).unwrap();
        assert!(matches!(
            pm.validate_request(0, 0, 16 * 1024 + 1),
            Err(PieceMapError::BlockTooLarge(_, _))
        ));
    }

    #[test]
    fn request_out_of_bounds() {
        let files = make_files(&[(&["a.bin"], 100)]);
        let pm = PieceMap::new(256 * 1024, files).unwrap();
        // piece 0 is only 100 bytes
        assert!(matches!(
            pm.validate_request(0, 90, 20),
            Err(PieceMapError::RequestOutOfBounds { .. })
        ));
    }

    #[test]
    fn valid_request_resolves_file_region() {
        let files = make_files(&[(&["a.bin"], 1024 * 1024)]);
        let pm = PieceMap::new(256 * 1024, files).unwrap();
        let regions = pm.validate_request(1, 0, 16 * 1024).unwrap();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].file_offset, 256 * 1024); // piece 1 starts at byte 256K
        assert_eq!(regions[0].length, 16 * 1024);
    }

    #[test]
    fn content_range_last_piece() {
        let files = make_files(&[(&["a.bin"], 300)]);
        let pm = PieceMap::new(256, files).unwrap();
        let (start, end) = pm.piece_content_range(1).unwrap();
        assert_eq!(start, 256);
        assert_eq!(end, 300);
    }

    #[cfg(test)]
    mod property {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn all_bytes_covered(
                total in 1u64..=(10 * 1024 * 1024u64),
                piece_length in 1u64..=262144u64,
            ) {
                let files = vec![FileSpan {
                    file_index: 0,
                    path: SafeRelPath::from_name("a.bin", false).unwrap(),
                    content_offset: 0,
                    length: total,
                }];
                let pm = PieceMap::new(piece_length, files).unwrap();
                let mut covered = 0u64;
                for p in 0..pm.piece_count {
                    let regions = pm.piece_to_file_regions(p).unwrap();
                    for r in regions {
                        covered += r.length;
                    }
                }
                prop_assert_eq!(covered, total);
            }

            #[test]
            fn piece_lengths_sum_to_total(
                total in 1u64..=(4 * 1024 * 1024u64),
                piece_length in 1u64..=65536u64,
            ) {
                let files = vec![FileSpan {
                    file_index: 0,
                    path: SafeRelPath::from_name("a.bin", false).unwrap(),
                    content_offset: 0,
                    length: total,
                }];
                let pm = PieceMap::new(piece_length, files).unwrap();
                let sum: u64 = (0..pm.piece_count)
                    .map(|p| pm.piece_len(p).unwrap() as u64)
                    .sum();
                prop_assert_eq!(sum, total);
            }
        }
    }
}
