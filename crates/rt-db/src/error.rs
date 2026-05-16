use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("migration failed at version {version}: {reason}")]
    Migration { version: u32, reason: String },

    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("record not found: {0}")]
    NotFound(String),
}
