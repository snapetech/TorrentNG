use std::{
    io,
    path::{Path, PathBuf},
};

use tracing::instrument;

use crate::{error::FastresumeError, state::FastresumeState};

/// A fast-resume record is metadata, not a torrent payload. Keep a corrupt or
/// operator-planted file from turning startup into an unbounded allocation
/// before JSON validation runs.
pub const MAX_FASTRESUME_BYTES: usize = 64 * 1024 * 1024;

/// Persists and loads `FastresumeState` as JSON files in a session directory.
///
/// Files are written atomically via a temp file + rename to avoid partial writes
/// that would corrupt the state on crash.
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
        let state: FastresumeState = serde_json::from_slice(&data)?;
        Ok(state)
    }

    /// Save fastresume state atomically.
    #[instrument(skip(self, state), fields(info_hash = %state.info_hash))]
    pub fn save(&self, state: &FastresumeState) -> Result<(), FastresumeError> {
        rt_storage::create_dir_all_no_follow(&self.dir)?;
        let target = self.checked_path_for(&state.info_hash)?;
        let tmp = target.with_extension("tmp");
        let data = serde_json::to_vec_pretty(state)?;
        rt_storage::write_file_no_follow_sync(&tmp, &data)?;
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
}
