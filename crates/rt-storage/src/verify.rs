use std::path::Path;

use rt_path::SafeRelPath;
use tracing::instrument;

use rt_piece_map::{FileRegion, PieceMap};

use crate::{
    error::StorageError,
    io_class::IoClass,
    scheduler::{scheduled_read_owned, MountScheduler},
};

/// Result of verifying a single piece.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyResult {
    /// SHA-1 matches the expected hash.
    Valid,
    /// SHA-1 does not match.
    Invalid,
    /// One or more files could not be read (missing, permission denied, etc.)
    Missing { file_index: u32, reason: String },
}

#[derive(Debug, Clone)]
pub struct V2FileHash {
    pub file_index: u32,
    pub path: SafeRelPath,
    pub length: u64,
    pub pieces_root: [u8; 32],
}

/// Verifies piece data against expected SHA-1 hashes by reading directly
/// from disk. Never uses mmap.
pub struct PieceVerifier<'a> {
    storage_root: &'a Path,
    scheduler: &'a MountScheduler,
    piece_map: &'a PieceMap,
    expected_hashes: &'a [[u8; 20]],
}

impl<'a> PieceVerifier<'a> {
    pub fn new(
        storage_root: &'a Path,
        scheduler: &'a MountScheduler,
        piece_map: &'a PieceMap,
        expected_hashes: &'a [[u8; 20]],
    ) -> Self {
        PieceVerifier {
            storage_root,
            scheduler,
            piece_map,
            expected_hashes,
        }
    }

    /// Verify a single piece. Reads all file regions that compose the piece.
    #[instrument(skip(self), fields(piece))]
    pub async fn verify_piece(&self, piece: u32) -> VerifyResult {
        let regions = match self.piece_map.piece_to_file_regions(piece) {
            Ok(r) => r,
            Err(e) => {
                return VerifyResult::Missing {
                    file_index: 0,
                    reason: e.to_string(),
                }
            }
        };

        let expected = match self.expected_hashes.get(piece as usize) {
            Some(h) => h,
            None => {
                return VerifyResult::Missing {
                    file_index: 0,
                    reason: format!("no hash for piece {piece}"),
                }
            }
        };

        let total_len = regions
            .iter()
            .map(|region| region.length as usize)
            .sum::<usize>();
        let mut piece_data = Vec::with_capacity(total_len);

        for region in &regions {
            match self.read_region(region).await {
                Ok(data) => piece_data.extend_from_slice(&data),
                Err(e) => {
                    tracing::warn!(
                        component = "storage",
                        operation = "verify_piece",
                        piece,
                        file_index = region.file_index,
                        result = "error",
                        error = %e,
                        "file read failed during verify"
                    );
                    return VerifyResult::Missing {
                        file_index: region.file_index,
                        reason: e.to_string(),
                    };
                }
            }
        }

        let actual = match self
            .scheduler
            .hash_sha1(bytes::Bytes::from(piece_data))
            .await
        {
            Ok(hash) => hash,
            Err(e) => {
                return VerifyResult::Missing {
                    file_index: 0,
                    reason: e.to_string(),
                }
            }
        };
        if &actual == expected {
            tracing::debug!(piece, "piece valid");
            VerifyResult::Valid
        } else {
            tracing::warn!(
                component = "storage",
                operation = "verify_piece",
                piece,
                result = "invalid",
                "piece hash mismatch"
            );
            VerifyResult::Invalid
        }
    }

    /// Verify all pieces, returning a bitset of valid pieces.
    pub async fn verify_all(&self) -> Vec<VerifyResult> {
        let mut results = Vec::with_capacity(self.piece_map.piece_count as usize);
        for piece in 0..self.piece_map.piece_count {
            results.push(self.verify_piece(piece).await);
        }
        results
    }

    /// Verify a range of pieces (for resumable recheck).
    pub async fn verify_range(
        &self,
        start: u32,
        end: u32,
    ) -> Result<Vec<(u32, VerifyResult)>, StorageError> {
        let end = end.min(self.piece_map.piece_count);
        let mut results = Vec::with_capacity((end - start) as usize);
        for piece in start..end {
            let result = self.verify_piece(piece).await;
            results.push((piece, result));
        }
        Ok(results)
    }

    async fn read_region(&self, region: &FileRegion) -> Result<bytes::Bytes, StorageError> {
        let file_path = region.path.resolve(self.storage_root);
        read_sparse_range(
            self.scheduler,
            &file_path,
            region.file_offset,
            region.length,
        )
        .await
    }
}

/// Verifies BEP 52 file roots by reading each file in 16 KiB leaves and
/// comparing the computed merkle root with the metainfo `pieces root`.
pub struct V2FileVerifier<'a> {
    storage_root: &'a Path,
    scheduler: &'a MountScheduler,
    files: &'a [V2FileHash],
}

impl<'a> V2FileVerifier<'a> {
    pub const LEAF_SIZE: usize = 16 * 1024;

    pub fn new(
        storage_root: &'a Path,
        scheduler: &'a MountScheduler,
        files: &'a [V2FileHash],
    ) -> Self {
        Self {
            storage_root,
            scheduler,
            files,
        }
    }

    pub async fn verify_all(&self) -> Vec<(u32, VerifyResult)> {
        let mut out = Vec::with_capacity(self.files.len());
        for file in self.files {
            out.push((file.file_index, self.verify_file(file).await));
        }
        out
    }

    #[instrument(skip(self, file), fields(file_index = file.file_index))]
    pub async fn verify_file(&self, file: &V2FileHash) -> VerifyResult {
        let actual = match self.file_root(file).await {
            Ok(root) => root,
            Err(e) => {
                tracing::warn!(
                    component = "storage",
                    operation = "verify_v2_file",
                    file_index = file.file_index,
                    result = "error",
                    error = %e,
                    "file read failed during v2 verify"
                );
                return VerifyResult::Missing {
                    file_index: file.file_index,
                    reason: e.to_string(),
                };
            }
        };
        if actual == file.pieces_root {
            VerifyResult::Valid
        } else {
            tracing::warn!(
                component = "storage",
                operation = "verify_v2_file",
                file_index = file.file_index,
                result = "invalid",
                "v2 file root mismatch"
            );
            VerifyResult::Invalid
        }
    }

    async fn file_root(&self, file: &V2FileHash) -> Result<[u8; 32], StorageError> {
        let path = file.path.resolve(self.storage_root);
        let mut leaves = Vec::with_capacity((file.length as usize).div_ceil(Self::LEAF_SIZE));
        let mut offset = 0u64;
        while offset < file.length {
            let len = (file.length - offset).min(Self::LEAF_SIZE as u64) as usize;
            let data = read_sparse_range(self.scheduler, &path, offset, len as u64).await?;
            leaves.push(self.scheduler.hash_v2_leaf(data).await?);
            offset += len as u64;
        }
        if leaves.is_empty() {
            leaves.push(self.scheduler.hash_v2_leaf(bytes::Bytes::new()).await?);
        }
        self.scheduler.hash_v2_root(leaves).await
    }
}

async fn read_sparse_range(
    scheduler: &MountScheduler,
    path: &Path,
    offset: u64,
    len: u64,
) -> Result<bytes::Bytes, StorageError> {
    let extents = scheduler.data_extents(path, offset, len).await?;
    if extents.is_empty() {
        return Ok(bytes::Bytes::from(vec![0; len as usize]));
    }

    let mut out = Vec::with_capacity(len as usize);
    let range_end = offset.saturating_add(len);
    let mut cursor = offset;
    for extent in extents {
        if extent.offset > cursor {
            out.resize(out.len() + (extent.offset - cursor) as usize, 0);
        }
        let extent_end = extent.offset.saturating_add(extent.len).min(range_end);
        if extent_end > extent.offset {
            let data = scheduled_read_owned(
                scheduler,
                IoClass::Recheck,
                path,
                extent.offset,
                (extent_end - extent.offset) as usize,
            )
            .await?;
            out.extend_from_slice(data.as_slice());
            cursor = extent_end;
        }
    }
    if cursor < range_end {
        out.resize(out.len() + (range_end - cursor) as usize, 0);
    }
    Ok(bytes::Bytes::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::scheduled_write;
    use rt_hash::{merkle_root, BlockHash};
    use rt_path::SafeRelPath;
    use rt_piece_map::{FileSpan, PieceMap};
    use sha1::{Digest, Sha1};

    use crate::scheduler::{MountScheduler, SchedulerConfig};
    use rt_path::{StorageProfile, StorageRootId};

    fn ssd_scheduler() -> MountScheduler {
        MountScheduler::new(
            StorageRootId::new(),
            &SchedulerConfig {
                profile: StorageProfile::Ssd,
                ..Default::default()
            },
        )
    }

    fn piece_hash(data: &[u8]) -> [u8; 20] {
        Sha1::digest(data).into()
    }

    fn v2_file_root(content: &[u8]) -> [u8; 32] {
        let mut leaves = Vec::new();
        for chunk in content.chunks(V2FileVerifier::LEAF_SIZE) {
            leaves.push(BlockHash::of(chunk).0);
        }
        if leaves.is_empty() {
            leaves.push(BlockHash::of(&[]).0);
        }
        merkle_root(&leaves)
    }

    #[tokio::test]
    async fn verify_valid_piece() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"hello world - this is exactly one piece of data!";
        let fname = "data.bin";
        std::fs::write(dir.path().join(fname), content).unwrap();

        let piece_length = content.len() as u64;
        let hash = piece_hash(content);

        let files = vec![FileSpan {
            file_index: 0,
            path: SafeRelPath::from_name(fname, false).unwrap(),
            content_offset: 0,
            length: piece_length,
        }];
        let pm = PieceMap::new(piece_length, files).unwrap();
        let sched = ssd_scheduler();
        let hashes = [hash];
        let verifier = PieceVerifier::new(dir.path(), &sched, &pm, &hashes);

        assert_eq!(verifier.verify_piece(0).await, VerifyResult::Valid);
    }

    #[tokio::test]
    async fn verify_sparse_piece_hashes_holes_as_zeroes() {
        let dir = tempfile::tempdir().unwrap();
        let fname = "sparse.bin";
        let path = dir.path().join(fname);
        let piece_len = 512 * 1024usize;
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(piece_len as u64).unwrap();
        drop(file);
        let data_offset = 128 * 1024usize;
        let data = vec![0x5au8; 4096];
        scheduled_write(
            &ssd_scheduler(),
            IoClass::PeerWrite,
            &path,
            data_offset as u64,
            bytes::Bytes::from(data.clone()),
            false,
        )
        .await
        .unwrap();

        let mut expected_piece = vec![0u8; piece_len];
        expected_piece[data_offset..data_offset + data.len()].copy_from_slice(&data);
        let hash = piece_hash(&expected_piece);
        let files = vec![FileSpan {
            file_index: 0,
            path: SafeRelPath::from_name(fname, false).unwrap(),
            content_offset: 0,
            length: piece_len as u64,
        }];
        let pm = PieceMap::new(piece_len as u64, files).unwrap();
        let sched = ssd_scheduler();
        let hashes = [hash];
        let verifier = PieceVerifier::new(dir.path(), &sched, &pm, &hashes);

        assert_eq!(verifier.verify_piece(0).await, VerifyResult::Valid);
    }

    #[tokio::test]
    async fn verify_truncated_sparse_file_is_missing_not_zero_filled() {
        let dir = tempfile::tempdir().unwrap();
        let fname = "truncated.bin";
        let path = dir.path().join(fname);
        std::fs::write(&path, [0u8; 1024]).unwrap();
        let piece_len = 128 * 1024usize;
        let expected = piece_hash(&vec![0u8; piece_len]);
        let files = vec![FileSpan {
            file_index: 0,
            path: SafeRelPath::from_name(fname, false).unwrap(),
            content_offset: 0,
            length: piece_len as u64,
        }];
        let pm = PieceMap::new(piece_len as u64, files).unwrap();
        let sched = ssd_scheduler();
        let hashes = [expected];
        let verifier = PieceVerifier::new(dir.path(), &sched, &pm, &hashes);

        assert!(matches!(
            verifier.verify_piece(0).await,
            VerifyResult::Missing { file_index: 0, .. }
        ));
    }

    #[tokio::test]
    async fn verify_invalid_piece() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"corrupted data!!!";
        let fname = "data.bin";
        std::fs::write(dir.path().join(fname), content).unwrap();

        let wrong_hash = [0u8; 20]; // definitely wrong
        let piece_length = content.len() as u64;

        let files = vec![FileSpan {
            file_index: 0,
            path: SafeRelPath::from_name(fname, false).unwrap(),
            content_offset: 0,
            length: piece_length,
        }];
        let pm = PieceMap::new(piece_length, files).unwrap();
        let sched = ssd_scheduler();
        let hashes = [wrong_hash];
        let verifier = PieceVerifier::new(dir.path(), &sched, &pm, &hashes);

        assert_eq!(verifier.verify_piece(0).await, VerifyResult::Invalid);
    }

    #[tokio::test]
    async fn verify_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        // don't create the file

        let files = vec![FileSpan {
            file_index: 0,
            path: SafeRelPath::from_name("missing.bin", false).unwrap(),
            content_offset: 0,
            length: 256,
        }];
        let pm = PieceMap::new(256, files).unwrap();
        let sched = ssd_scheduler();
        let hashes = [[0u8; 20]];
        let verifier = PieceVerifier::new(dir.path(), &sched, &pm, &hashes);

        assert!(matches!(
            verifier.verify_piece(0).await,
            VerifyResult::Missing { .. }
        ));
    }

    #[tokio::test]
    async fn verify_all_reports_per_piece() {
        let dir = tempfile::tempdir().unwrap();
        // Two-piece file: piece 0 valid, piece 1 has wrong hash
        let piece_len = 64usize;
        let content: Vec<u8> = (0..piece_len * 2).map(|i| i as u8).collect();
        std::fs::write(dir.path().join("data.bin"), &content).unwrap();

        let hash0 = piece_hash(&content[..piece_len]);
        let hash1_wrong = [0u8; 20];

        let files = vec![FileSpan {
            file_index: 0,
            path: SafeRelPath::from_name("data.bin", false).unwrap(),
            content_offset: 0,
            length: content.len() as u64,
        }];
        let pm = PieceMap::new(piece_len as u64, files).unwrap();
        let sched = ssd_scheduler();
        let hashes = [hash0, hash1_wrong];
        let verifier = PieceVerifier::new(dir.path(), &sched, &pm, &hashes);

        let results = verifier.verify_all().await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], VerifyResult::Valid);
        assert_eq!(results[1], VerifyResult::Invalid);
    }

    #[tokio::test]
    async fn verify_range_resumable() {
        let dir = tempfile::tempdir().unwrap();
        let piece_len = 32usize;
        let n_pieces = 4usize;
        let content: Vec<u8> = (0..(piece_len * n_pieces)).map(|i| i as u8).collect();
        std::fs::write(dir.path().join("data.bin"), &content).unwrap();

        let hashes: Vec<[u8; 20]> = (0..n_pieces)
            .map(|i| piece_hash(&content[i * piece_len..(i + 1) * piece_len]))
            .collect();

        let files = vec![FileSpan {
            file_index: 0,
            path: SafeRelPath::from_name("data.bin", false).unwrap(),
            content_offset: 0,
            length: content.len() as u64,
        }];
        let pm = PieceMap::new(piece_len as u64, files).unwrap();
        let sched = ssd_scheduler();
        let verifier = PieceVerifier::new(dir.path(), &sched, &pm, &hashes);

        // Verify only pieces 1..3 (simulating resume from piece 1)
        let results = verifier.verify_range(1, 3).await.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, 1);
        assert_eq!(results[0].1, VerifyResult::Valid);
        assert_eq!(results[1].0, 2);
        assert_eq!(results[1].1, VerifyResult::Valid);
    }

    #[tokio::test]
    async fn v2_file_verify_accepts_matching_file_root() {
        let dir = tempfile::tempdir().unwrap();
        let content: Vec<u8> = (0..(V2FileVerifier::LEAF_SIZE + 17))
            .map(|idx| idx as u8)
            .collect();
        std::fs::write(dir.path().join("data.bin"), &content).unwrap();

        let file = V2FileHash {
            file_index: 3,
            path: SafeRelPath::from_name("data.bin", false).unwrap(),
            length: content.len() as u64,
            pieces_root: v2_file_root(&content),
        };
        let sched = ssd_scheduler();
        let verifier = V2FileVerifier::new(dir.path(), &sched, std::slice::from_ref(&file));

        assert_eq!(verifier.verify_file(&file).await, VerifyResult::Valid);
        assert_eq!(
            verifier.verify_all().await,
            vec![(file.file_index, VerifyResult::Valid)]
        );
    }

    #[tokio::test]
    async fn v2_file_verify_hashes_sparse_holes_as_zeroes() {
        let dir = tempfile::tempdir().unwrap();
        let fname = "sparse-v2.bin";
        let path = dir.path().join(fname);
        let file_len = (V2FileVerifier::LEAF_SIZE * 2) as u64;
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(file_len).unwrap();
        drop(file);
        let data_offset = V2FileVerifier::LEAF_SIZE + 2048;
        let data = vec![0xa5u8; 4096];
        scheduled_write(
            &ssd_scheduler(),
            IoClass::PeerWrite,
            &path,
            data_offset as u64,
            bytes::Bytes::from(data.clone()),
            false,
        )
        .await
        .unwrap();

        let mut expected = vec![0u8; file_len as usize];
        expected[data_offset..data_offset + data.len()].copy_from_slice(&data);
        let file = V2FileHash {
            file_index: 7,
            path: SafeRelPath::from_name(fname, false).unwrap(),
            length: file_len,
            pieces_root: v2_file_root(&expected),
        };
        let sched = ssd_scheduler();
        let verifier = V2FileVerifier::new(dir.path(), &sched, std::slice::from_ref(&file));

        assert_eq!(verifier.verify_file(&file).await, VerifyResult::Valid);
    }

    #[tokio::test]
    async fn v2_file_verify_rejects_wrong_file_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.bin"), b"content").unwrap();
        let file = V2FileHash {
            file_index: 0,
            path: SafeRelPath::from_name("data.bin", false).unwrap(),
            length: 7,
            pieces_root: [0u8; 32],
        };
        let sched = ssd_scheduler();
        let verifier = V2FileVerifier::new(dir.path(), &sched, std::slice::from_ref(&file));

        assert_eq!(verifier.verify_file(&file).await, VerifyResult::Invalid);
    }
}
