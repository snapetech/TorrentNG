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

    /// Whether the error shows the filesystem does not implement the
    /// requested operation at all (some FUSE and network mounts answer `fsync`
    /// with `ENOSYS`/`EINVAL`/`EOPNOTSUPP`; on Windows `FlushFileBuffers`
    /// answers `ERROR_INVALID_FUNCTION`/`ERROR_NOT_SUPPORTED`), as opposed to a
    /// transient or media I/O error that is worth retrying.
    pub fn is_operation_unsupported(&self) -> bool {
        let StorageError::Io { source, .. } = self else {
            return false;
        };
        let Some(code) = source.raw_os_error() else {
            return false;
        };
        #[cfg(unix)]
        {
            matches!(code, libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP)
        }
        #[cfg(windows)]
        {
            // ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_CALL_NOT_IMPLEMENTED
            matches!(code, 1 | 50 | 120)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = code;
            false
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// OS error codes that mean "this filesystem cannot do that", and codes
    /// that mean a real I/O problem, for the platform under test.
    #[cfg(unix)]
    const UNSUPPORTED: [i32; 3] = [libc::ENOSYS, libc::EINVAL, libc::EOPNOTSUPP];
    #[cfg(unix)]
    const REAL_ERRORS: [i32; 3] = [libc::EIO, libc::ENOSPC, libc::EBADF];
    #[cfg(windows)]
    const UNSUPPORTED: [i32; 3] = [1, 50, 120];
    #[cfg(windows)]
    const REAL_ERRORS: [i32; 3] = [5, 112, 23]; // access denied, disk full, CRC error

    #[cfg(any(unix, windows))]
    #[test]
    fn unsupported_operation_errnos_are_distinguished_from_media_errors() {
        for code in UNSUPPORTED {
            let err = StorageError::Io {
                path: "p".to_owned(),
                source: std::io::Error::from_raw_os_error(code),
            };
            assert!(err.is_operation_unsupported(), "errno {code}");
        }
        for code in REAL_ERRORS {
            let err = StorageError::Io {
                path: "p".to_owned(),
                source: std::io::Error::from_raw_os_error(code),
            };
            assert!(!err.is_operation_unsupported(), "errno {code}");
        }
        assert!(!StorageError::Cancelled.is_operation_unsupported());
    }
}
