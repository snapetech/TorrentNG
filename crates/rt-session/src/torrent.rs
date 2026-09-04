use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::SessionError, state::TorrentState};

/// Unique session handle for a torrent (not the infohash — used for in-memory tracking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TorrentHandle(pub Uuid);

impl TorrentHandle {
    pub fn new() -> Self {
        TorrentHandle(Uuid::new_v4())
    }
}

impl Default for TorrentHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Upload/download accounting for ratio tracking.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferStats {
    pub uploaded: u64,
    pub downloaded: u64,
}

impl TransferStats {
    pub fn ratio(&self) -> f64 {
        if self.downloaded == 0 {
            return 0.0;
        }
        self.uploaded as f64 / self.downloaded as f64
    }

    pub fn add_upload(&mut self, bytes: u64) {
        self.uploaded = self.uploaded.saturating_add(bytes);
    }

    pub fn add_download(&mut self, bytes: u64) {
        self.downloaded = self.downloaded.saturating_add(bytes);
    }
}

/// All runtime state for one torrent in the session registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentEntry {
    pub handle: TorrentHandle,
    /// Hex-encoded SHA-1 infohash.
    pub info_hash: String,
    pub name: String,
    pub save_path: String,
    pub total_length: u64,
    pub amount_left: u64,
    pub state: TorrentState,
    pub stats: TransferStats,
    pub added_at: u64,
    pub completed_at: Option<u64>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub error_message: Option<String>,
    /// The active tracker's failure/warning message, if any, independent
    /// of `state`/`error_message` -- a torrent can be actively seeding or
    /// downloading fine while its tracker rejects announces (e.g. "torrent
    /// not registered with this tracker"). Updated whenever a tracker
    /// announce completes; cleared on the next successful announce.
    #[serde(default)]
    pub tracker_message: Option<String>,
}

/// The durable projection retained for a torrent that has no live runtime
/// task.  A dormant torrent must remain addressable by the APIs, but it must
/// not retain the task-only error/message allocations or a second owned copy
/// of every immutable string.  Mutating a dormant torrent promotes it to a
/// [`TorrentEntry`] at the registry boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DormantTorrent {
    pub(crate) handle: TorrentHandle,
    pub(crate) info_hash: Arc<str>,
    pub(crate) name: Arc<str>,
    pub(crate) save_path: Arc<str>,
    pub(crate) total_length: u64,
    pub(crate) amount_left: u64,
    pub(crate) state: TorrentState,
    pub(crate) stats: TransferStats,
    pub(crate) added_at: u64,
    pub(crate) completed_at: Option<u64>,
    pub(crate) category: Option<Arc<str>>,
    pub(crate) tags: Arc<[String]>,
    /// Externally visible messages retained compactly while the runtime task
    /// is dormant. Dropping these would make a projection report a different
    /// torrent after a snapshot refresh or restart.
    pub(crate) error_message: Option<Arc<str>>,
    pub(crate) tracker_message: Option<Arc<str>>,
}

impl TorrentEntry {
    /// Move an entry into the compact dormant representation.  The runtime
    /// registry uses this when demoting a taskless torrent; no metainfo or
    /// task-owned data is retained here.
    pub fn into_dormant(self) -> DormantTorrent {
        DormantTorrent {
            handle: self.handle,
            info_hash: Arc::<str>::from(self.info_hash),
            name: Arc::<str>::from(self.name),
            save_path: Arc::<str>::from(self.save_path),
            total_length: self.total_length,
            amount_left: self.amount_left,
            state: self.state,
            stats: self.stats,
            added_at: self.added_at,
            completed_at: self.completed_at,
            category: self.category.map(Arc::<str>::from),
            tags: Arc::<[String]>::from(self.tags.into_boxed_slice()),
            error_message: self.error_message.map(Arc::<str>::from),
            tracker_message: self.tracker_message.map(Arc::<str>::from),
        }
    }
}

impl DormantTorrent {
    /// Materialize the API/runtime projection when a caller needs a normal
    /// mutable entry or an owned list item.  This is deliberately lazy: the
    /// idle registry itself stores `DormantTorrent`, not this expansion.
    pub fn to_entry(&self) -> TorrentEntry {
        TorrentEntry {
            handle: self.handle,
            info_hash: self.info_hash.to_string(),
            name: self.name.to_string(),
            save_path: self.save_path.to_string(),
            total_length: self.total_length,
            amount_left: self.amount_left,
            state: self.state,
            stats: self.stats.clone(),
            added_at: self.added_at,
            completed_at: self.completed_at,
            category: self.category.as_ref().map(|value| value.to_string()),
            tags: self.tags.as_ref().to_vec(),
            error_message: self.error_message.as_ref().map(|value| value.to_string()),
            tracker_message: self.tracker_message.as_ref().map(|value| value.to_string()),
        }
    }

    pub(crate) fn contribution(&self) -> (TorrentState, u64, u64, u64) {
        (
            self.state,
            self.stats.uploaded,
            self.stats.downloaded,
            self.amount_left,
        )
    }
}

impl TorrentEntry {
    pub fn new(info_hash: String, name: String, save_path: String) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        TorrentEntry {
            handle: TorrentHandle::new(),
            info_hash,
            name,
            save_path,
            total_length: 0,
            amount_left: 0,
            state: TorrentState::Stopped,
            stats: TransferStats::default(),
            added_at: now,
            completed_at: None,
            category: None,
            tags: Vec::new(),
            error_message: None,
            tracker_message: None,
        }
    }

    pub fn transition(&mut self, target: TorrentState) -> Result<(), SessionError> {
        if self.state == target {
            return Ok(());
        }
        let valid = match (self.state, target) {
            (TorrentState::Stopped, TorrentState::Checking) => true,
            (TorrentState::Stopped, TorrentState::MetadataPending) => true,
            (TorrentState::Stopped, TorrentState::Downloading) => true,
            (TorrentState::Stopped, TorrentState::Paused) => true,
            (TorrentState::MetadataPending, TorrentState::Checking) => true,
            (TorrentState::MetadataPending, TorrentState::Downloading) => true,
            (TorrentState::MetadataPending, TorrentState::Paused) => true,
            (TorrentState::MetadataPending, TorrentState::Error) => true,
            (TorrentState::Paused, TorrentState::Checking) => true,
            (TorrentState::Paused, TorrentState::MetadataPending) => true,
            (TorrentState::Paused, TorrentState::Downloading) => true,
            (TorrentState::Paused, TorrentState::Seeding) => true,
            (TorrentState::Checking, TorrentState::Seeding) => true,
            (TorrentState::Checking, TorrentState::Downloading) => true,
            (TorrentState::Checking, TorrentState::Paused) => true,
            (TorrentState::Checking, TorrentState::Stopped) => true,
            (TorrentState::Checking, TorrentState::Error) => true,
            (TorrentState::Downloading, TorrentState::Seeding) => true,
            (TorrentState::Downloading, TorrentState::Paused) => true,
            (TorrentState::Downloading, TorrentState::Error) => true,
            (TorrentState::Seeding, TorrentState::Paused) => true,
            (TorrentState::Seeding, TorrentState::Stopped) => true,
            (TorrentState::Seeding, TorrentState::Error) => true,
            // A recheck can be run against an already-seeding torrent (the
            // existing `TorrentCmd::Recheck` command permits this
            // regardless of current state, and TNG-002's post-storage-move
            // recheck relies on it too): without these, `set_state`'s
            // `entry.transition(...)` call was silently rejected the
            // instant a recheck of a seeding torrent tried to report
            // `Checking` or, on finding something invalid, `Downloading`
            // -- the registry's state field stayed stuck on the stale
            // `Seeding` value no matter what the recheck actually found.
            (TorrentState::Seeding, TorrentState::Checking) => true,
            (TorrentState::Seeding, TorrentState::Downloading) => true,
            (TorrentState::Queued, TorrentState::Downloading) => true,
            (TorrentState::Queued, TorrentState::Seeding) => true,
            (TorrentState::Queued, TorrentState::Paused) => true,
            (_, TorrentState::Stopped) if self.state != TorrentState::Stopped => true,
            _ => false,
        };
        if !valid {
            return Err(SessionError::InvalidTransition {
                from: self.state,
                to: target,
            });
        }
        self.state = target;
        if target == TorrentState::Error {
            // preserve error_message set by caller
        }
        Ok(())
    }

    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.state = TorrentState::Error;
        self.error_message = Some(msg.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> TorrentEntry {
        TorrentEntry::new("a".repeat(40), "test".into(), "/data".into())
    }

    #[test]
    fn starts_stopped() {
        let e = entry();
        assert_eq!(e.state, TorrentState::Stopped);
    }

    #[test]
    fn stopped_to_checking() {
        let mut e = entry();
        e.transition(TorrentState::Checking).unwrap();
        assert_eq!(e.state, TorrentState::Checking);
    }

    #[test]
    fn checking_to_seeding() {
        let mut e = entry();
        e.transition(TorrentState::Checking).unwrap();
        e.transition(TorrentState::Seeding).unwrap();
        assert_eq!(e.state, TorrentState::Seeding);
    }

    #[test]
    fn seeding_to_paused() {
        let mut e = entry();
        e.transition(TorrentState::Checking).unwrap();
        e.transition(TorrentState::Seeding).unwrap();
        e.transition(TorrentState::Paused).unwrap();
        assert_eq!(e.state, TorrentState::Paused);
    }

    #[test]
    fn same_state_transition_is_idempotent() {
        let mut e = entry();
        e.transition(TorrentState::Stopped).unwrap();
        assert_eq!(e.state, TorrentState::Stopped);
    }

    #[test]
    fn seeding_torrent_can_be_rechecked() {
        // A recheck must be able to move a seeding torrent back through
        // Checking and, if it finds something wrong, to Downloading --
        // otherwise `set_state`'s silently-discarded `transition()` result
        // leaves the registry reporting a stale "Seeding" no matter what a
        // later recheck actually finds (real bug, caught while writing a
        // TNG-002 test for post-move rechecks).
        let mut e = entry();
        e.transition(TorrentState::Checking).unwrap();
        e.transition(TorrentState::Seeding).unwrap();

        e.transition(TorrentState::Checking).unwrap();
        assert_eq!(e.state, TorrentState::Checking);

        e.transition(TorrentState::Downloading).unwrap();
        assert_eq!(e.state, TorrentState::Downloading);
    }

    #[test]
    fn invalid_transition_errors() {
        let mut e = entry();
        let err = e.transition(TorrentState::Seeding).unwrap_err();
        assert!(matches!(err, SessionError::InvalidTransition { .. }));
    }

    #[test]
    fn ratio_calculation() {
        let mut stats = TransferStats::default();
        stats.add_upload(2_000_000);
        stats.add_download(1_000_000);
        assert!((stats.ratio() - 2.0).abs() < 0.001);
    }

    #[test]
    fn ratio_zero_when_no_download() {
        let stats = TransferStats::default();
        assert_eq!(stats.ratio(), 0.0);
    }

    #[test]
    fn set_error_sets_state_and_message() {
        let mut e = entry();
        e.set_error("disk full");
        assert_eq!(e.state, TorrentState::Error);
        assert_eq!(e.error_message.as_deref(), Some("disk full"));
    }
}
