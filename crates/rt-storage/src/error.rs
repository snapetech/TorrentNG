use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("disk full on storage root {root}: needed {needed} bytes, {available} available")]
    DiskFull {
        root: String,
        needed: u64,
        available: u64,
    },
    #[error("permission denied: {path}")]
    PermissionDenied { path: String },
    #[error("file not found: {path}")]
    FileNotFound { path: String },
    #[error("operation cancelled")]
    Cancelled,
    #[error("scheduler queue full (mount: {mount})")]
    QueueFull { mount: String },
    #[error("staged move failed at step {step}: {reason}")]
    StagedMoveFailed { step: &'static str, reason: String },
    #[error("path error: {0}")]
    Path(#[from] rt_path::PathError),
}

impl StorageError {
    pub fn io(path: impl Into<String>, source: std::io::Error) -> Self {
        let path = path.into();
        if source.kind() == std::io::ErrorKind::PermissionDenied {
            StorageError::PermissionDenied { path }
        } else if source.kind() == std::io::ErrorKind::NotFound {
            StorageError::FileNotFound { path }
        } else {
            StorageError::Io { path, source }
        }
    }
}
