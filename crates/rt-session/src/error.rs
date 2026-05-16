use thiserror::Error;

use crate::state::TorrentState;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("torrent not found: {0}")]
    NotFound(String),

    #[error("torrent already exists: {0}")]
    AlreadyExists(String),

    #[error("invalid state transition from {from} to {to}")]
    InvalidTransition {
        from: TorrentState,
        to: TorrentState,
    },
}
