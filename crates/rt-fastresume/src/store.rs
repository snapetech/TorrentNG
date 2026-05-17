use std::path::{Path, PathBuf};

use tracing::instrument;

use crate::{error::FastresumeError, state::FastresumeState};

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

    /// Load fastresume state for the given infohash.
    #[instrument(skip(self), fields(info_hash = info_hash_hex))]
    pub fn load(&self, info_hash_hex: &str) -> Result<FastresumeState, FastresumeError> {
        let path = self.path_for(info_hash_hex);
        let data = std::fs::read(&path).map_err(|e| {
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
        std::fs::create_dir_all(&self.dir)?;
        let target = self.path_for(&state.info_hash);
        let tmp = target.with_extension("tmp");
        let data = serde_json::to_vec_pretty(state)?;
        std::fs::write(&tmp, &data)?;
        std::fs::rename(&tmp, &target)?;
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
        let path = self.path_for(info_hash_hex);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(FastresumeError::Io(e)),
        }
    }

    /// True if a fastresume file exists for the given infohash.
    pub fn exists(&self, info_hash_hex: &str) -> bool {
        self.path_for(info_hash_hex).exists()
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
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
        store.delete("nonexistent").unwrap();
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
}
