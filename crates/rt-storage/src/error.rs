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
    #[error("short I/O on {path}: expected {expected} bytes, got {actual}")]
    ShortIo {
        path: String,
        expected: usize,
        actual: usize,
    },
    #[error("scheduler queue full (mount: {mount})")]
    QueueFull { mount: String },
    #[error("staged move failed at step {step}: {reason}")]
    StagedMoveFailed { step: &'static str, reason: String },
    #[error("storage filesystem state requires manual recovery at step {step}: {reason}")]
    FilesystemStateUncertain { step: &'static str, reason: String },
    #[error("path error: {0}")]
    Path(#[from] rt_path::PathError),
    #[error(
        "refusing to shrink {path}: on-disk size {current_len} exceeds requested length {requested_len}"
    )]
    RefuseShrink {
        path: String,
        current_len: u64,
        requested_len: u64,
    },
}

impl StorageError {
    /// Return whether the error means the executor cannot prove that the
    /// filesystem is back in a state where an owning torrent may safely run.
    pub fn requires_manual_recovery(&self) -> bool {
        matches!(self, Self::FilesystemStateUncertain { .. })
    }

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
