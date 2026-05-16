use serde::{Deserialize, Serialize};

/// Lifecycle state of a torrent managed by the session daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TorrentState {
    /// Added but not yet started.
    Stopped,
    /// Checking piece hashes.
    Checking,
    /// Seeding (all pieces verified).
    Seeding,
    /// Downloading missing pieces.
    Downloading,
    /// Paused by user (was seeding or downloading).
    Paused,
    /// Queued, waiting for a download/seed slot.
    Queued,
    /// Terminal error state.
    Error,
}

impl TorrentState {
    pub fn is_terminal(self) -> bool {
        matches!(self, TorrentState::Error)
    }

    pub fn can_start(self) -> bool {
        matches!(
            self,
            TorrentState::Stopped | TorrentState::Paused | TorrentState::Error
        )
    }

    pub fn can_pause(self) -> bool {
        matches!(
            self,
            TorrentState::Seeding | TorrentState::Downloading | TorrentState::Queued
        )
    }

    pub fn can_check(self) -> bool {
        matches!(
            self,
            TorrentState::Stopped
                | TorrentState::Paused
                | TorrentState::Seeding
                | TorrentState::Downloading
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TorrentState::Stopped => "stopped",
            TorrentState::Checking => "checking",
            TorrentState::Seeding => "seeding",
            TorrentState::Downloading => "downloading",
            TorrentState::Paused => "paused",
            TorrentState::Queued => "queued",
            TorrentState::Error => "error",
        }
    }
}

impl std::fmt::Display for TorrentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_start_stopped() {
        assert!(TorrentState::Stopped.can_start());
        assert!(TorrentState::Paused.can_start());
        assert!(!TorrentState::Seeding.can_start());
        assert!(!TorrentState::Checking.can_start());
    }

    #[test]
    fn can_pause_active_states() {
        assert!(TorrentState::Seeding.can_pause());
        assert!(TorrentState::Downloading.can_pause());
        assert!(!TorrentState::Stopped.can_pause());
        assert!(!TorrentState::Checking.can_pause());
    }

    #[test]
    fn error_is_terminal() {
        assert!(TorrentState::Error.is_terminal());
        assert!(!TorrentState::Seeding.is_terminal());
    }

    #[test]
    fn display_matches_serde() {
        assert_eq!(TorrentState::Seeding.as_str(), "seeding");
        assert_eq!(TorrentState::Downloading.as_str(), "downloading");
    }
}
