use std::time::{SystemTime, UNIX_EPOCH};

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
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
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
    pub state: TorrentState,
    pub stats: TransferStats,
    pub added_at: u64,
    pub completed_at: Option<u64>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub error_message: Option<String>,
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
            state: TorrentState::Stopped,
            stats: TransferStats::default(),
            added_at: now,
            completed_at: None,
            category: None,
            tags: Vec::new(),
            error_message: None,
        }
    }

    pub fn transition(&mut self, target: TorrentState) -> Result<(), SessionError> {
        let valid = match (self.state, target) {
            (TorrentState::Stopped, TorrentState::Checking) => true,
            (TorrentState::Stopped, TorrentState::Downloading) => true,
            (TorrentState::Stopped, TorrentState::Paused) => true,
            (TorrentState::Paused, TorrentState::Checking) => true,
            (TorrentState::Paused, TorrentState::Downloading) => true,
            (TorrentState::Paused, TorrentState::Seeding) => true,
            (TorrentState::Checking, TorrentState::Seeding) => true,
            (TorrentState::Checking, TorrentState::Downloading) => true,
            (TorrentState::Checking, TorrentState::Error) => true,
            (TorrentState::Downloading, TorrentState::Seeding) => true,
            (TorrentState::Downloading, TorrentState::Paused) => true,
            (TorrentState::Downloading, TorrentState::Error) => true,
            (TorrentState::Seeding, TorrentState::Paused) => true,
            (TorrentState::Seeding, TorrentState::Stopped) => true,
            (TorrentState::Seeding, TorrentState::Error) => true,
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
