use thiserror::Error;

#[derive(Debug, Error)]
pub enum FastresumeError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("fastresume version mismatch: expected {expected}, got {found}")]
    VersionMismatch { expected: u32, found: u32 },
    #[error("infohash mismatch: stored {stored}, current {current}")]
    InfohashMismatch { stored: String, current: String },
    #[error("piece count mismatch: stored {stored}, current {current}")]
    PieceCountMismatch { stored: u32, current: u32 },
    #[error("fastresume file not found")]
    NotFound,
}
