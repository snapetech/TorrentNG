use std::path::Path;

use futures::future;
use rt_hash::MerkleAccumulator;
use rt_path::SafeRelPath;
use sha1::{Digest as Sha1Digest, Sha1};
use tracing::instrument;

use rt_piece_map::{FileRegion, PieceMap};

use crate::{
    error::StorageError,
    io_class::IoClass,
    scheduler::{scheduled_read_owned, MountScheduler},
};

const VERIFY_READ_CHUNK_BYTES: u64 = 1024 * 1024;

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
    pub pieces_root: Option<[u8; 32]>,
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
    ///
    /// Chunk reads and chunk hashing are pipelined (double-buffered): while
    /// chunk N is being hashed on the dedicated hash pool, chunk N+1 is
    /// already being read from disk, so the wall-clock cost of a piece is
    /// closer to `max(read_time, hash_time)` than `sum(read_time, hash_time)`.
    /// The very first read and the very last hash are not overlapped with
    /// anything (pipeline prologue/epilogue), which is the normal shape of
    /// software pipelining.
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

        // Flatten the piece's file regions into a fixed read-chunk plan up
        // front (region index, file offset, length) so the pipeline below
        // can look one chunk ahead, including across a file-region boundary.
        let mut chunks: Vec<(usize, u64, u64)> = Vec::new();
        for (region_index, region) in regions.iter().enumerate() {
            let mut region_offset = 0u64;
            while region_offset < region.length {
                let len = (region.length - region_offset).min(VERIFY_READ_CHUNK_BYTES);
                let file_offset = match region.file_offset.checked_add(region_offset) {
                    Some(offset) => offset,
                    None => {
                        return VerifyResult::Missing {
                            file_index: region.file_index,
                            reason: "piece region offset overflow".to_owned(),
                        };
                    }
                };
                chunks.push((region_index, file_offset, len));
                region_offset += len;
            }
        }

        let mut hasher = Sha1::new();

        // Prologue: start the first chunk's read. There is nothing to
        // overlap it with yet.
        let mut pending = match chunks.first() {
            Some(&(region_index, file_offset, len)) => {
                let region = &regions[region_index];
                match self.read_region_chunk(region, file_offset, len).await {
                    Ok(data) => Some(data),
                    Err(e) => {
                        self.log_chunk_read_failure(piece, region.file_index, &e);
                        return VerifyResult::Missing {
                            file_index: region.file_index,
                            reason: e.to_string(),
                        };
                    }
                }
            }
            None => None,
        };

        for (i, &(region_index, _, _)) in chunks.iter().enumerate() {
            let data = pending
                .take()
                .expect("verify pipeline lost its buffered chunk");
            match chunks.get(i + 1) {
                Some(&(next_region_index, next_offset, next_len)) => {
                    // Steady state: hash the chunk in hand while prefetching
                    // the next one. Both futures are polled concurrently by
                    // `future::join`, so the next chunk's disk read overlaps
                    // the current chunk's SHA-1 update on the hash pool.
                    let next_region = &regions[next_region_index];
                    let hash_fut = recheck_hash_sha1_update(self.scheduler, &hasher, &data);
                    let read_fut = self.read_region_chunk(next_region, next_offset, next_len);
                    let (hash_result, read_result) = future::join(hash_fut, read_fut).await;
                    hasher = match hash_result {
                        Ok(updated) => updated,
                        Err(e) => {
                            return VerifyResult::Missing {
                                file_index: regions[region_index].file_index,
                                reason: e.to_string(),
                            };
                        }
                    };
                    pending = match read_result {
                        Ok(data) => Some(data),
                        Err(e) => {
                            self.log_chunk_read_failure(piece, next_region.file_index, &e);
                            return VerifyResult::Missing {
                                file_index: next_region.file_index,
                                reason: e.to_string(),
                            };
                        }
                    };
                }
                None => {
                    // Epilogue: the last chunk has nothing left to prefetch.
                    match recheck_hash_sha1_update(self.scheduler, &hasher, &data).await {
                        Ok(updated) => hasher = updated,
                        Err(e) => {
                            return VerifyResult::Missing {
                                file_index: regions[region_index].file_index,
                                reason: e.to_string(),
                            };
                        }
                    }
                }
            }
        }

        let actual: [u8; 20] = hasher.finalize().into();
        if &actual == expected {
            tracing::debug!(
                component = "storage",
                operation = "verify_piece",
                piece,
                result = "valid",
                "piece valid"
            );
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
        let start = start.min(self.piece_map.piece_count);
        let end = end.min(self.piece_map.piece_count);
        if start >= end {
            return Ok(Vec::new());
        }
        let mut results = Vec::with_capacity((end - start) as usize);
        for piece in start..end {
            let result = self.verify_piece(piece).await;
            results.push((piece, result));
        }
        Ok(results)
    }

    async fn read_region_chunk(
        &self,
        region: &FileRegion,
        file_offset: u64,
        len: u64,
    ) -> Result<bytes::Bytes, StorageError> {
        let file_path = region.path.resolve(self.storage_root);
        read_sparse_range(self.scheduler, &file_path, file_offset, len).await
    }

    fn log_chunk_read_failure(&self, piece: u32, file_index: u32, error: &StorageError) {
        tracing::warn!(
            component = "storage",
            operation = "verify_piece",
            piece,
            file_index,
            result = "error",
            error = %error,
            "file read failed during verify"
        );
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
        if file.pieces_root.is_none() && file.length == 0 {
            // BEP 52 omits `pieces root` for empty files. `file_root` still
            // read the path, so a missing file is reported as Missing above.
            VerifyResult::Valid
        } else if Some(actual) == file.pieces_root {
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
        if file.length == 0 {
            // Empty BEP 52 files omit `pieces root`, but the payload path must
            // still exist. A zero-length scheduled read validates that the
            // path can be opened without allocating a data buffer.
            let _ = recheck_read_owned(self.scheduler, &path, 0, 0).await?;
        }
        let mut accumulator = MerkleAccumulator::new();
        let mut offset = 0u64;
        while offset < file.length {
            let len = (file.length - offset).min(Self::LEAF_SIZE as u64) as usize;
            let data = read_sparse_range(self.scheduler, &path, offset, len as u64).await?;
            accumulator.push(recheck_hash_v2_leaf(self.scheduler, &data).await?);
            offset += len as u64;
        }
        if file.length == 0 {
            let empty = bytes::Bytes::new();
            accumulator.push(recheck_hash_v2_leaf(self.scheduler, &empty).await?);
        }
        Ok(accumulator.finish())
    }
}

async fn read_sparse_range(
    scheduler: &MountScheduler,
    path: &Path,
    offset: u64,
    len: u64,
) -> Result<bytes::Bytes, StorageError> {
    let extents = recheck_data_extents(scheduler, path, offset, len).await?;
    if extents.is_empty() {
        return recheck_read_owned(scheduler, path, offset, len as usize).await;
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
            let data = recheck_read_owned(
                scheduler,
                path,
                extent.offset,
                (extent_end - extent.offset) as usize,
            )
            .await?;
            out.extend_from_slice(&data);
            cursor = extent_end;
        }
    }
    if cursor < range_end {
        out.resize(out.len() + (range_end - cursor) as usize, 0);
    }
    Ok(bytes::Bytes::from(out))
}

async fn recheck_read_owned(
    scheduler: &MountScheduler,
    path: &Path,
    offset: u64,
    len: usize,
) -> Result<bytes::Bytes, StorageError> {
    let mut attempts = 0;
    loop {
        match scheduled_read_owned(scheduler, IoClass::Recheck, path, offset, len).await {
            Ok(read) => return Ok(read.into_bytes()),
            Err(error @ StorageError::QueueFull { .. }) => {
                if !retry_recheck_queue_full(&mut attempts).await {
                    return Err(error);
                }
            }
            Err(err) => return Err(err),
        }
    }
}

async fn recheck_hash_sha1_update(
    scheduler: &MountScheduler,
    hasher: &Sha1,
    data: &bytes::Bytes,
) -> Result<Sha1, StorageError> {
    let mut attempts = 0;
    loop {
        match scheduler.hash_sha1_update(hasher, data.clone()).await {
            Ok(updated) => return Ok(updated),
            Err(error @ StorageError::QueueFull { .. }) => {
                if !retry_recheck_queue_full(&mut attempts).await {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

async fn recheck_hash_v2_leaf(
    scheduler: &MountScheduler,
    data: &bytes::Bytes,
) -> Result<[u8; 32], StorageError> {
    let mut attempts = 0;
    loop {
        match scheduler.hash_v2_leaf(data.clone()).await {
            Ok(hash) => return Ok(hash),
            Err(error @ StorageError::QueueFull { .. }) => {
                if !retry_recheck_queue_full(&mut attempts).await {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

const MAX_RECHECK_QUEUE_RETRIES: usize = 10_000;

async fn retry_recheck_queue_full(attempts: &mut usize) -> bool {
    if *attempts >= MAX_RECHECK_QUEUE_RETRIES {
        return false;
    }
    *attempts += 1;
    if (*attempts).is_multiple_of(16) {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    } else {
        tokio::task::yield_now().await;
    }
    true
}

async fn recheck_data_extents(
    scheduler: &MountScheduler,
    path: &Path,
    offset: u64,
    len: u64,
) -> Result<Vec<crate::scheduler::DataExtent>, StorageError> {
    let mut attempts = 0;
    loop {
        match scheduler.data_extents(path, offset, len).await {
            Ok(extents) => return Ok(extents),
            Err(error @ StorageError::QueueFull { .. }) => {
                if !retry_recheck_queue_full(&mut attempts).await {
                    return Err(error);
                }
            }
            Err(err) => return Err(err),
        }
    }
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
    async fn verify_piece_streams_large_piece() {
        let dir = tempfile::tempdir().unwrap();
        let fname = "large-sparse.bin";
        let piece_len = VERIFY_READ_CHUNK_BYTES * 2 + 17;
        let file = std::fs::File::create(dir.path().join(fname)).unwrap();
        file.set_len(piece_len).unwrap();
        drop(file);

        let zero_chunk = [0u8; 4096];
        let mut expected_hasher = Sha1::new();
        let mut remaining = piece_len;
        while remaining > 0 {
            let len = remaining.min(zero_chunk.len() as u64) as usize;
            expected_hasher.update(&zero_chunk[..len]);
            remaining -= len as u64;
        }
        let hash: [u8; 20] = expected_hasher.finalize().into();
        let files = vec![FileSpan {
            file_index: 0,
            path: SafeRelPath::from_name(fname, false).unwrap(),
            content_offset: 0,
            length: piece_len,
        }];
        let pm = PieceMap::new(piece_len, files).unwrap();
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
    async fn verify_range_returns_empty_for_reversed_or_out_of_range_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![FileSpan {
            file_index: 0,
            path: SafeRelPath::from_name("data.bin", false).unwrap(),
            content_offset: 0,
            length: 32,
        }];
        let pm = PieceMap::new(32, files).unwrap();
        let sched = ssd_scheduler();
        let hashes = [[0u8; 20]];
        let verifier = PieceVerifier::new(dir.path(), &sched, &pm, &hashes);

        assert!(verifier.verify_range(1, 0).await.unwrap().is_empty());
        assert!(verifier
            .verify_range(u32::MAX, u32::MAX)
            .await
            .unwrap()
            .is_empty());
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
            pieces_root: Some(v2_file_root(&content)),
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
            pieces_root: Some(v2_file_root(&expected)),
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
            pieces_root: Some([0u8; 32]),
        };
        let sched = ssd_scheduler();
        let verifier = V2FileVerifier::new(dir.path(), &sched, std::slice::from_ref(&file));

        assert_eq!(verifier.verify_file(&file).await, VerifyResult::Invalid);
    }

    #[tokio::test]
    async fn v2_file_verify_accepts_empty_file_without_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("empty.bin"), []).unwrap();
        let file = V2FileHash {
            file_index: 2,
            path: SafeRelPath::from_name("empty.bin", false).unwrap(),
            length: 0,
            pieces_root: None,
        };
        let sched = ssd_scheduler();
        let verifier = V2FileVerifier::new(dir.path(), &sched, std::slice::from_ref(&file));

        assert_eq!(verifier.verify_file(&file).await, VerifyResult::Valid);
    }

    #[tokio::test]
    async fn v2_file_verify_rejects_missing_empty_file_without_root() {
        let dir = tempfile::tempdir().unwrap();
        let file = V2FileHash {
            file_index: 3,
            path: SafeRelPath::from_name("missing-empty.bin", false).unwrap(),
            length: 0,
            pieces_root: None,
        };
        let sched = ssd_scheduler();
        let verifier = V2FileVerifier::new(dir.path(), &sched, std::slice::from_ref(&file));

        assert!(matches!(
            verifier.verify_file(&file).await,
            VerifyResult::Missing { file_index: 3, .. }
        ));
    }
}
