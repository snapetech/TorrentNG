//! Process-wide storage runtime.
//!
//! Wires the three Storage NG primitives together — the open-handle cache,
//! the bounded frame pool, and the disk backend — behind one accessor.
//! Resources are *global* by design: a single fd budget and a single byte
//! budget shared by every torrent, so cost scales with active transfer,
//! not with torrent count.
//!
//! Configuration is read from the environment at first use (defaults are
//! sized for a large seedbox):
//!
//! - `TNG_STORAGE_FRAME_CAP_MB`     frame-pool hard cap (default 256)
//! - `TNG_STORAGE_BACKEND`          auto | pread | uring (default auto)
//! - `TNG_STORAGE_DISK_THREADS`     backend worker threads (default: cores/2)
//! - `TNG_STORAGE_HANDLE_IDLE_SECS` idle handle TTL      (default 30)

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use once_cell::sync::Lazy;

use crate::backend::{
    BackendKind, BackendRequest, DiskBackend, FixedBufferStrategy, SelectedDiskBackend,
};
use crate::error::StorageError;
use crate::fd_limit::{handle_cache_capacity, raise_nofile_limit};
use crate::frame::{global_frame_pool, Frame, FramePool};
use crate::handle_cache::HandleCache;

const DEFAULT_HANDLE_IDLE_SECS: u64 = 30;
/// How often the background sweeper closes idle handles.
const SWEEP_INTERVAL: Duration = Duration::from_secs(5);

/// Shared storage runtime: handle cache + frame pool + disk backend.
pub struct StorageRuntime {
    handles: HandleCache,
    frames: FramePool,
    backend: SelectedDiskBackend,
}

impl StorageRuntime {
    fn from_env() -> Self {
        let soft_nofile = raise_nofile_limit();
        let cap = handle_cache_capacity(soft_nofile);

        let idle_secs = env_u64("TNG_STORAGE_HANDLE_IDLE_SECS", DEFAULT_HANDLE_IDLE_SECS);
        let handles = HandleCache::new(cap, Duration::from_secs(idle_secs));
        let frames = global_frame_pool().clone();
        let backend_request = std::env::var("TNG_STORAGE_BACKEND")
            .ok()
            .map(|value| BackendRequest::parse(&value))
            .unwrap_or(BackendRequest::Auto);
        let backend = match std::env::var("TNG_STORAGE_DISK_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
        {
            Some(n) if n > 0 => SelectedDiskBackend::select(backend_request, n),
            _ => SelectedDiskBackend::select_default(backend_request),
        };

        // Background idle-handle sweeper. A plain OS thread so it works
        // regardless of whether a Tokio runtime exists at init time.
        let sweep_handles = handles.clone();
        std::thread::Builder::new()
            .name("tng-handle-sweep".to_string())
            .spawn(move || loop {
                std::thread::sleep(SWEEP_INTERVAL);
                let closed = sweep_handles.sweep_idle();
                if closed > 0 {
                    tracing::debug!(
                        component = "storage",
                        operation = "sweep_idle_handles",
                        closed,
                        result = "ok",
                        "idle file handles closed"
                    );
                }
            })
            .expect("spawn handle sweeper");

        tracing::info!(
            component = "storage",
            operation = "init_runtime",
            handle_cap = cap,
            frame_cap_bytes = frames.cap_bytes(),
            idle_secs,
            backend = backend.kind().as_str(),
            backend_reason = %backend.selection().reason,
            result = "ok",
            "storage runtime initialised"
        );

        StorageRuntime {
            handles,
            frames,
            backend,
        }
    }

    /// The process-wide runtime, initialised on first access.
    pub fn global() -> &'static StorageRuntime {
        static RT: Lazy<StorageRuntime> = Lazy::new(StorageRuntime::from_env);
        &RT
    }

    /// Open file handles currently cached.
    pub fn handles_open(&self) -> usize {
        self.handles.len()
    }

    /// Frame-pool bytes currently checked out.
    pub fn frame_in_use_bytes(&self) -> u64 {
        self.frames.in_use_bytes()
    }

    /// Frame-pool acquisition attempts denied because the hard cap was reached.
    pub fn frame_denied_allocations(&self) -> u64 {
        self.frames.denied_allocations()
    }

    /// Frame-pool hard cap.
    pub fn frame_cap_bytes(&self) -> u64 {
        self.frames.cap_bytes()
    }

    pub fn backend_kind(&self) -> BackendKind {
        self.backend.kind()
    }

    pub fn backend_reason(&self) -> &str {
        &self.backend.selection().reason
    }

    pub fn backend_supports_fixed_buffers(&self) -> bool {
        self.backend.supports_fixed_buffers()
    }

    pub fn backend_supports_registered_files(&self) -> bool {
        self.backend.supports_registered_files()
    }

    pub fn backend_max_batch_len(&self) -> usize {
        self.backend.max_batch_len()
    }

    pub fn backend_fixed_buffer_len(&self) -> usize {
        self.backend.fixed_buffer_len()
    }

    pub fn backend_fixed_buffer_strategy(&self) -> FixedBufferStrategy {
        self.backend.fixed_buffer_strategy()
    }

    fn open_read(&self, path: &Path) -> Result<Arc<File>, StorageError> {
        self.handles
            .get_or_open(path, false, false)
            .map(|h| h.file())
            .map_err(|e| StorageError::io(path.display().to_string(), e))
    }

    fn open_write(&self, path: &Path, create: bool) -> Result<Arc<File>, StorageError> {
        self.handles
            .get_or_open(path, true, create)
            .map(|h| h.file())
            .map_err(|e| StorageError::io(path.display().to_string(), e))
    }

    /// Positioned read of `len` bytes at `offset`, into a pooled frame.
    /// Returns [`StorageError::QueueFull`] when the frame cap is reached
    /// (the caller must apply peer backpressure rather than retry-spin).
    pub async fn read_frame(
        &self,
        path: &Path,
        offset: u64,
        len: usize,
    ) -> Result<Frame, StorageError> {
        let file = self.open_read(path)?;
        let frame = self
            .frames
            .try_acquire(len)
            .ok_or_else(|| StorageError::QueueFull {
                mount: "frame-pool".to_string(),
            })?;
        match self.backend.pread(file, frame, offset).await {
            Ok(Ok(frame)) => Ok(frame),
            Ok(Err(e)) => Err(map_backend_io_error(path, e, Some(len))),
            Err(_) => Err(StorageError::Cancelled),
        }
    }

    /// Positioned write of `data` at `offset`. Creates the file when
    /// `create` is set (parent directory must already exist).
    pub async fn write_at(
        &self,
        path: &Path,
        offset: u64,
        data: bytes::Bytes,
        create: bool,
    ) -> Result<(), StorageError> {
        let file = self.open_write(path, create)?;
        let len = data.len();
        match self.backend.pwrite(file, data, offset).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(map_backend_io_error(path, e, Some(len))),
            Err(_) => Err(StorageError::Cancelled),
        }
    }
}

fn map_backend_io_error(
    path: &Path,
    error: std::io::Error,
    expected_len: Option<usize>,
) -> StorageError {
    if error.kind() == std::io::ErrorKind::WouldBlock {
        StorageError::QueueFull {
            mount: "storage-backend".to_string(),
        }
    } else if matches!(
        error.kind(),
        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::WriteZero
    ) {
        StorageError::ShortIo {
            path: path.display().to_string(),
            expected: expected_len.unwrap_or(0),
            actual: 0,
        }
    } else {
        StorageError::io(path.display().to_string(), error)
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn global_read_write_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rt.bin");
        std::fs::write(&path, vec![0u8; 32]).unwrap();
        let rt = StorageRuntime::global();

        rt.write_at(&path, 4, bytes::Bytes::from_static(b"NGIO"), false)
            .await
            .unwrap();
        let frame = rt.read_frame(&path, 4, 4).await.unwrap();
        assert_eq!(frame.as_slice(), b"NGIO");
    }

    #[tokio::test]
    async fn missing_file_maps_to_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let rt = StorageRuntime::global();
        let err = rt
            .read_frame(&dir.path().join("absent.bin"), 0, 8)
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::FileNotFound { .. }));
    }

    #[test]
    fn backend_would_block_maps_to_queue_full() {
        let err = map_backend_io_error(
            Path::new("/storage/root/file.bin"),
            std::io::Error::new(std::io::ErrorKind::WouldBlock, "storage backend queue full"),
            None,
        );
        assert!(matches!(
            err,
            StorageError::QueueFull { ref mount } if mount == "storage-backend"
        ));
    }

    #[test]
    fn backend_short_io_maps_to_storage_error() {
        let err = map_backend_io_error(
            Path::new("/storage/root/file.bin"),
            std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short read"),
            Some(4096),
        );
        assert!(matches!(
            err,
            StorageError::ShortIo {
                expected: 4096,
                actual: 0,
                ..
            }
        ));
    }
}
