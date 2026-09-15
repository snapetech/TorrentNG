use std::{
    io,
    path::{Path, PathBuf},
};

use tracing::instrument;

use crate::{
    error::FastresumeError,
    state::{FastresumeState, PieceState, MAX_FASTRESUME_PIECES},
};

/// A fast-resume record is metadata, not a torrent payload. Keep a corrupt or
/// operator-planted file from turning startup into an unbounded allocation
/// before JSON validation runs.
pub const MAX_FASTRESUME_BYTES: usize = 64 * 1024 * 1024;

/// Marks the start of the packed-bitfield container format (format 2). This
/// can never collide with the legacy format: every legacy file is JSON
/// produced by `serde_json::to_writer_pretty` for a top-level struct, so its
/// first byte is always `{` (0x7B), never `R` (0x52).
const CONTAINER_MAGIC: [u8; 4] = *b"RTF2";

/// Version of the *container framing* below (magic + header + bitfield
/// trailer), independent of `FastresumeState::version` (the schema version
/// carried inside the JSON header itself). Bump this if the trailer layout
/// ever changes again.
const CONTAINER_VERSION: u8 = 1;

/// Persists and loads `FastresumeState` in a session directory.
///
/// On-disk format ("format 2"): a small JSON header holding every field
/// except `pieces`, followed by `pieces` packed as one bit per piece
/// (MSB-first within each byte, matching the BEP3 wire-bitfield convention
/// already used elsewhere in this project for interop, e.g.
/// `rt_migrate::libtorrent_piece_states`) instead of one JSON string per
/// piece. A bit of `1` means [`PieceState::Valid`]; `0` covers `Unknown`,
/// `Invalid`, and `Missing` alike, which is lossless in practice because
/// every consumer in this codebase already treats those three states
/// identically (recheck-required) and none of them ever constructs
/// `Invalid`/`Missing` today.
///
/// Files previously written as plain pretty-printed JSON (one string per
/// piece) are still read transparently: [`FastresumeStore::load`] detects
/// the shape from the first bytes of the file and falls back to the legacy
/// decode path, so upgrading does not force a full-library recheck.
///
/// Writes always use the new packed format. Files are written atomically via
/// a temp file + rename to avoid partial writes that would corrupt the state
/// on crash.
pub struct FastresumeStore {
    dir: PathBuf,
}

impl FastresumeStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        FastresumeStore { dir: dir.into() }
    }

    fn path_for(&self, info_hash_hex: &str) -> PathBuf {
        self.dir.join(format!("{info_hash_hex}.fastresume.json"))
    }

    fn checked_path_for(&self, info_hash_hex: &str) -> Result<PathBuf, FastresumeError> {
        if !is_safe_hash_component(info_hash_hex) {
            return Err(FastresumeError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fastresume infohash must be a non-empty hexadecimal filename component",
            )));
        }
        Ok(self.path_for(info_hash_hex))
    }

    /// Load fastresume state for the given infohash.
    ///
    /// Transparently reads either the current packed-bitfield container
    /// format or a legacy pretty-JSON file written by an older build.
    #[instrument(skip(self), fields(info_hash = info_hash_hex))]
    pub fn load(&self, info_hash_hex: &str) -> Result<FastresumeState, FastresumeError> {
        let path = self.checked_path_for(info_hash_hex)?;
        let data = read_bounded_no_follow(&path, MAX_FASTRESUME_BYTES).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                FastresumeError::NotFound
            } else {
                FastresumeError::Io(e)
            }
        })?;
        if data.starts_with(&CONTAINER_MAGIC) {
            decode_container(&data)
        } else {
            Ok(serde_json::from_slice::<FastresumeState>(&data)?)
        }
    }

    /// Save fastresume state atomically. Blocking: does synchronous file I/O
    /// and a double fsync (data file + containing directory) on the calling
    /// thread.
    ///
    /// This is the primitive used by non-async callers (e.g. `rt-migrate`,
    /// which has no tokio dependency). From an async/tokio context, prefer
    /// [`FastresumeStore::save_async`], which runs this same work on a
    /// blocking-pool thread instead of stalling the calling task.
    #[instrument(skip(self, state), fields(info_hash = %state.info_hash))]
    pub fn save(&self, state: &FastresumeState) -> Result<(), FastresumeError> {
        rt_storage::create_dir_all_no_follow(&self.dir)?;
        let target = self.checked_path_for(&state.info_hash)?;
        let tmp = target.with_extension("tmp");
        let bytes = encode_container(state)?;
        if bytes.len() > MAX_FASTRESUME_BYTES {
            return Err(FastresumeError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("serialized fastresume exceeds the {MAX_FASTRESUME_BYTES} byte limit"),
            )));
        }
        rt_storage::write_file_no_follow_sync(&tmp, &bytes)?;
        rt_storage::rename_no_follow(&tmp, &target)?;
        if let Some(parent) = target.parent() {
            rt_storage::sync_dir_no_follow(parent)?;
        }
        tracing::debug!(
            component = "fastresume",
            operation = "save",
            torrent = %state.info_hash,
            result = "ok",
            "fastresume saved"
        );
        Ok(())
    }

    /// Async equivalent of [`FastresumeStore::save`]: runs the same blocking
    /// file I/O and double fsync via `tokio::task::spawn_blocking` so the
    /// calling task's worker thread is never stalled on it. Intended for the
    /// hot recheck path (a save roughly every 64 pieces), which previously
    /// called the blocking `save` directly from async code.
    pub async fn save_async(&self, state: FastresumeState) -> Result<(), FastresumeError> {
        let dir = self.dir.clone();
        tokio::task::spawn_blocking(move || FastresumeStore { dir }.save(&state))
            .await
            .unwrap_or_else(|join_error| Err(FastresumeError::Io(io::Error::other(join_error))))
    }

    /// Delete fastresume state (on torrent removal).
    pub fn delete(&self, info_hash_hex: &str) -> Result<(), FastresumeError> {
        let path = self.checked_path_for(info_hash_hex)?;
        match rt_storage::remove_file_no_follow(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(FastresumeError::Io(e)),
        }
    }

    /// True if a fastresume file exists for the given infohash.
    pub fn exists(&self, info_hash_hex: &str) -> bool {
        self.checked_path_for(info_hash_hex)
            .is_ok_and(|path| rt_storage::metadata_no_follow(&path).is_ok())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Packs piece states into one bit per piece, MSB-first within each byte
/// (bit `i` lives at byte `i / 8`, mask `0x80 >> (i % 8)`) — the same
/// convention as a BEP3 wire bitfield and the modern libtorrent resume-file
/// encoding this project already decodes in `rt_migrate`. Only
/// [`PieceState::Valid`] sets a bit; every other state (`Unknown`,
/// `Invalid`, `Missing`) clears it.
fn encode_piece_bitfield(pieces: &[PieceState]) -> Vec<u8> {
    let mut bits = vec![0u8; pieces.len().div_ceil(8)];
    for (index, state) in pieces.iter().enumerate() {
        if *state == PieceState::Valid {
            bits[index / 8] |= 0x80u8 >> (index % 8);
        }
    }
    bits
}

/// Inverse of [`encode_piece_bitfield`]. A cleared bit decodes to
/// `PieceState::Unknown`: this is lossless for anything this codebase
/// actually writes today, since `Invalid`/`Missing` are never constructed
/// anywhere and every call site that matches on `PieceState` already
/// treats `Invalid | Missing | Unknown` identically (all three mean "not
/// verified, needs (re)check").
fn decode_piece_bitfield(bits: &[u8], piece_count: usize) -> Vec<PieceState> {
    let mut pieces = Vec::with_capacity(piece_count);
    for index in 0..piece_count {
        let byte = bits.get(index / 8).copied().unwrap_or(0);
        let mask = 0x80u8 >> (index % 8);
        pieces.push(if byte & mask != 0 {
            PieceState::Valid
        } else {
            PieceState::Unknown
        });
    }
    pieces
}

/// Encodes a `FastresumeState` in the packed-bitfield container format:
/// `MAGIC (4) | container version (1) | header_len: u32 LE (4) | header
/// JSON (header_len) | piece_count: u32 LE (4) | packed bitfield
/// (ceil(piece_count / 8))`.
///
/// The header JSON is everything in `FastresumeState` *except* `pieces` —
/// that field is stripped out and replaced by the packed trailer, which is
/// the entire point: a 50,000-piece torrent used to need ~50,000 JSON
/// strings for `pieces` and now needs 6,250 raw bytes.
fn encode_container(state: &FastresumeState) -> Result<Vec<u8>, FastresumeError> {
    let piece_count = u32::try_from(state.pieces.len()).map_err(|_| {
        FastresumeError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "fastresume piece count exceeds u32",
        ))
    })?;

    let mut header_value = serde_json::to_value(state)?;
    if let Some(object) = header_value.as_object_mut() {
        object.remove("pieces");
    }
    let header_json = serde_json::to_vec(&header_value)?;
    let header_len = u32::try_from(header_json.len()).map_err(|_| {
        FastresumeError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "fastresume header exceeds u32 bytes",
        ))
    })?;

    let bitfield = encode_piece_bitfield(&state.pieces);

    let mut out =
        Vec::with_capacity(CONTAINER_MAGIC.len() + 1 + 4 + header_json.len() + 4 + bitfield.len());
    out.extend_from_slice(&CONTAINER_MAGIC);
    out.push(CONTAINER_VERSION);
    out.extend_from_slice(&header_len.to_le_bytes());
    out.extend_from_slice(&header_json);
    out.extend_from_slice(&piece_count.to_le_bytes());
    out.extend_from_slice(&bitfield);
    Ok(out)
}

/// Decodes the packed-bitfield container format written by
/// [`encode_container`]. Reuses `FastresumeState`'s existing (bounded)
/// `Deserialize` impl for every field but `pieces`: the decoded bitfield is
/// re-inserted into the parsed header as a normal JSON array of piece-state
/// strings before the final `serde_json::from_value`, so all the existing
/// field validation (e.g. bounded partial-piece/file-hint vectors) still
/// applies unchanged.
fn decode_container(data: &[u8]) -> Result<FastresumeState, FastresumeError> {
    fn corrupt(message: &str) -> FastresumeError {
        FastresumeError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("corrupt fastresume container: {message}"),
        ))
    }

    let rest = data
        .get(CONTAINER_MAGIC.len()..)
        .ok_or_else(|| corrupt("truncated before container version"))?;
    let (&version, rest) = rest
        .split_first()
        .ok_or_else(|| corrupt("missing container version"))?;
    if version != CONTAINER_VERSION {
        return Err(corrupt("unsupported container version"));
    }

    let (len_bytes, rest) = rest
        .split_at_checked(4)
        .ok_or_else(|| corrupt("truncated header length"))?;
    let header_len = u32::from_le_bytes(len_bytes.try_into().expect("checked 4 bytes")) as usize;
    if header_len > rest.len() {
        return Err(corrupt("header length exceeds file size"));
    }
    let (header_json, rest) = rest.split_at(header_len);
    let mut header_value: serde_json::Value = serde_json::from_slice(header_json)?;

    let (count_bytes, rest) = rest
        .split_at_checked(4)
        .ok_or_else(|| corrupt("truncated piece count"))?;
    let piece_count = u32::from_le_bytes(count_bytes.try_into().expect("checked 4 bytes"));
    if piece_count as usize > MAX_FASTRESUME_PIECES {
        return Err(corrupt("piece count exceeds maximum"));
    }
    let expected_bitfield_len = (piece_count as usize).div_ceil(8);
    if rest.len() != expected_bitfield_len {
        return Err(corrupt("bitfield length does not match piece count"));
    }

    let pieces = decode_piece_bitfield(rest, piece_count as usize);
    let pieces_value = serde_json::to_value(&pieces)?;
    header_value
        .as_object_mut()
        .ok_or_else(|| corrupt("header is not a JSON object"))?
        .insert("pieces".to_string(), pieces_value);

    Ok(serde_json::from_value(header_value)?)
}

fn is_safe_hash_component(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn read_bounded_no_follow(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    rt_storage::read_file_no_follow_limited(path, max_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{FastresumeState, ImportPolicy, PieceState};

    fn test_hash_hex() -> String {
        hex::encode([3u8; 20])
    }

    fn make_state() -> FastresumeState {
        let mut s = FastresumeState::new_empty(&[3u8; 20], 4, ImportPolicy::RequireVerification);
        s.pieces[0] = PieceState::Valid;
        s.clean_shutdown = true;
        s
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FastresumeStore::new(dir.path());
        let state = make_state();

        store.save(&state).unwrap();
        assert!(store.exists(&test_hash_hex()));

        let loaded = store.load(&test_hash_hex()).unwrap();
        assert_eq!(loaded.info_hash, state.info_hash);
        assert_eq!(loaded.pieces[0], PieceState::Valid);
        assert_eq!(loaded.pieces[1], PieceState::Unknown);
        assert!(loaded.clean_shutdown);
    }

    #[test]
    fn load_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = FastresumeStore::new(dir.path());
        assert!(matches!(
            store.load("deadbeef"),
            Err(FastresumeError::NotFound)
        ));
    }

    #[test]
    fn delete_existing() {
        let dir = tempfile::tempdir().unwrap();
        let store = FastresumeStore::new(dir.path());
        let state = make_state();
        store.save(&state).unwrap();
        assert!(store.exists(&test_hash_hex()));
        store.delete(&test_hash_hex()).unwrap();
        assert!(!store.exists(&test_hash_hex()));
    }

    #[test]
    fn delete_nonexistent_ok() {
        let dir = tempfile::tempdir().unwrap();
        let store = FastresumeStore::new(dir.path());
        // Should not error
        store.delete("deadbeef").unwrap();
    }

    #[test]
    fn validate_loaded_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = FastresumeStore::new(dir.path());
        let state = make_state();
        store.save(&state).unwrap();

        let loaded = store.load(&test_hash_hex()).unwrap();
        assert!(loaded.validate(&[3u8; 20], 4).is_ok());
        assert!(loaded.validate(&[4u8; 20], 4).is_err());
    }

    #[test]
    fn atomic_write_no_partial() {
        // Verify that the .tmp file does not linger after a successful save
        let dir = tempfile::tempdir().unwrap();
        let store = FastresumeStore::new(dir.path());
        let state = make_state();
        store.save(&state).unwrap();

        let tmp = store.path_for(&test_hash_hex()).with_extension("tmp");
        assert!(
            !tmp.exists(),
            ".tmp file should not exist after successful save"
        );
    }

    #[test]
    fn rejects_path_like_infohash() {
        let dir = tempfile::tempdir().unwrap();
        let store = FastresumeStore::new(dir.path());

        assert!(matches!(
            store.load("../outside"),
            Err(FastresumeError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidInput
        ));
        assert!(!store.exists("../outside"));
    }

    #[test]
    fn bounded_read_rejects_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, b"1234").unwrap();

        let error = read_bounded_no_follow(&path, 3).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[test]
    fn save_rejects_an_ancestor_symlink_without_writing_outside() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let alias = root.path().join("fastresume");
        symlink(outside.path(), &alias).unwrap();

        let store = FastresumeStore::new(&alias);
        let error = store.save(&make_state()).unwrap_err();

        assert!(matches!(error, FastresumeError::Io(_)));
        assert!(!outside
            .path()
            .join(format!("{}.fastresume.json", test_hash_hex()))
            .exists());
    }

    #[test]
    fn new_format_pieces_round_trip_byte_correct() {
        // A piece count that isn't a multiple of 8 exercises the padding
        // bits in the final byte of the packed bitfield.
        let piece_count = 13u32;
        let hash = [7u8; 20];
        let mut state =
            FastresumeState::new_empty(&hash, piece_count, ImportPolicy::RequireVerification);
        for (index, piece) in state.pieces.iter_mut().enumerate() {
            *piece = if index % 3 == 0 {
                PieceState::Valid
            } else {
                PieceState::Unknown
            };
        }
        state.clean_shutdown = true;
        state.uploaded_bytes = 12345;
        state.downloaded_bytes = 6789;

        let dir = tempfile::tempdir().unwrap();
        let store = FastresumeStore::new(dir.path());
        store.save(&state).unwrap();

        let on_disk = std::fs::read(store.path_for(&hex::encode(hash))).unwrap();
        assert!(
            on_disk.starts_with(&CONTAINER_MAGIC),
            "save() must always write the new packed-bitfield container format"
        );

        let loaded = store.load(&hex::encode(hash)).unwrap();
        assert_eq!(loaded.pieces, state.pieces);
        assert_eq!(loaded.uploaded_bytes, state.uploaded_bytes);
        assert_eq!(loaded.downloaded_bytes, state.downloaded_bytes);
        assert!(loaded.clean_shutdown);
    }

    #[test]
    fn reads_legacy_json_array_of_strings_format() {
        // Reconstruct exactly what the pre-bitfield `save()` used to write:
        // `serde_json::to_writer_pretty` of the whole struct. This still
        // serializes `pieces` as one JSON string per piece today because
        // `FastresumeState`'s `Serialize` derive was deliberately left
        // untouched by the format-2 migration (only `store.rs`'s on-disk
        // framing changed), so this fixture needs no separate golden file.
        let hash = [8u8; 20];
        let mut legacy = FastresumeState::new_empty(&hash, 5, ImportPolicy::TrustHints);
        legacy.pieces[1] = PieceState::Valid;
        legacy.pieces[3] = PieceState::Valid;
        legacy.clean_shutdown = true;
        legacy.uploaded_bytes = 111;
        legacy.downloaded_bytes = 222;
        let legacy_bytes = serde_json::to_vec_pretty(&legacy).unwrap();
        assert!(
            legacy_bytes.starts_with(b"{"),
            "sanity check: legacy fixture must be plain JSON, not the new container"
        );

        let dir = tempfile::tempdir().unwrap();
        let store = FastresumeStore::new(dir.path());
        std::fs::write(store.path_for(&hex::encode(hash)), &legacy_bytes).unwrap();

        let loaded = store.load(&hex::encode(hash)).unwrap();
        assert_eq!(loaded.pieces, legacy.pieces);
        assert_eq!(loaded.uploaded_bytes, 111);
        assert_eq!(loaded.downloaded_bytes, 222);
        assert!(loaded.clean_shutdown);

        // Re-saving upgrades the file to the new format on disk, in place,
        // without discarding or resetting any decoded piece state — no
        // forced full-library recheck on upgrade.
        store.save(&loaded).unwrap();
        let upgraded = std::fs::read(store.path_for(&hex::encode(hash))).unwrap();
        assert!(upgraded.starts_with(&CONTAINER_MAGIC));
        assert_eq!(
            store.load(&hex::encode(hash)).unwrap().pieces,
            legacy.pieces
        );
    }

    #[test]
    fn new_format_is_much_smaller_than_legacy_format() {
        let piece_count = 50_000u32;
        let hash = [9u8; 20];
        let mut state =
            FastresumeState::new_empty(&hash, piece_count, ImportPolicy::RequireVerification);
        for (index, piece) in state.pieces.iter_mut().enumerate() {
            *piece = if index % 2 == 0 {
                PieceState::Valid
            } else {
                PieceState::Unknown
            };
        }

        let legacy_size = serde_json::to_vec_pretty(&state).unwrap().len();

        let dir = tempfile::tempdir().unwrap();
        let store = FastresumeStore::new(dir.path());
        store.save(&state).unwrap();
        let new_size = std::fs::metadata(store.path_for(&hex::encode(hash)))
            .unwrap()
            .len() as usize;

        // The task's own reference numbers (~50,000 JSON strings vs. ~6,250
        // packed bytes) put this around 60-80x. Assert a conservative lower
        // bound so this doesn't flake on incidental header-size changes.
        assert!(
            new_size.saturating_mul(10) < legacy_size,
            "expected new format ({new_size} bytes) to be at least 10x smaller than legacy ({legacy_size} bytes)"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_async_offloads_blocking_work_via_spawn_blocking() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let dir = tempfile::tempdir().unwrap();
        let store = FastresumeStore::new(dir.path());
        // Large enough that the blocking work (bitfield encode + file write
        // + double fsync) takes measurably longer than a few cooperative
        // yields, so the ticker task below can reliably observe interleaving.
        let mut state =
            FastresumeState::new_empty(&[10u8; 20], 2_000_000, ImportPolicy::RequireVerification);
        state.pieces.fill(PieceState::Valid);

        let ticks = Arc::new(AtomicUsize::new(0));
        let ticker_ticks = Arc::clone(&ticks);
        let ticker = tokio::spawn(async move {
            loop {
                ticker_ticks.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
            }
        });

        // If `save_async` ran the blocking I/O inline instead of via
        // `spawn_blocking`, this single-threaded runtime would have no
        // opportunity to poll `ticker` at all before this call resolves, so
        // `ticks` would still be at (or near) zero by the time it does.
        store.save_async(state).await.unwrap();
        ticker.abort();

        assert!(
            ticks.load(Ordering::SeqCst) > 0,
            "save_async must not block the current-thread runtime; the ticker task never ran, \
             which means the blocking I/O executed inline instead of via spawn_blocking"
        );
    }
}
