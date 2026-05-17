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
//! - `RTNG_STORAGE_FRAME_CAP_MB`     frame-pool hard cap (default 256)
//! - `RTNG_STORAGE_DISK_THREADS`     pread worker threads (default: cores/2)
//! - `RTNG_STORAGE_HANDLE_IDLE_SECS` idle handle TTL      (default 30)

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use once_cell::sync::Lazy;

use crate::backend::{DiskBackend, PreadBackend};
use crate::error::StorageError;
use crate::fd_limit::{handle_cache_capacity, raise_nofile_limit};
use crate::frame::{Frame, FramePool};
use crate::handle_cache::HandleCache;

const DEFAULT_FRAME_CAP_MB: u64 = 256;
const DEFAULT_HANDLE_IDLE_SECS: u64 = 30;
/// How often the background sweeper closes idle handles.
const SWEEP_INTERVAL: Duration = Duration::from_secs(5);

/// Shared storage runtime: handle cache + frame pool + disk backend.
pub struct StorageRuntime {
    handles: HandleCache,
    frames: FramePool,
    backend: PreadBackend,
}

impl StorageRuntime {
    fn from_env() -> Self {
        let soft_nofile = raise_nofile_limit();
        let cap = handle_cache_capacity(soft_nofile);

        let idle_secs = env_u64("RTNG_STORAGE_HANDLE_IDLE_SECS", DEFAULT_HANDLE_IDLE_SECS);
        let frame_cap_mb = env_u64("RTNG_STORAGE_FRAME_CAP_MB", DEFAULT_FRAME_CAP_MB);

        let handles = HandleCache::new(cap, Duration::from_secs(idle_secs));
        let frames = FramePool::new(frame_cap_mb.saturating_mul(1024 * 1024));
        let backend = match std::env::var("RTNG_STORAGE_DISK_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
        {
            Some(n) if n > 0 => PreadBackend::new(n),
            _ => PreadBackend::with_default_threads(),
        };

        // Background idle-handle sweeper. A plain OS thread so it works
        // regardless of whether a Tokio runtime exists at init time.
        let sweep_handles = handles.clone();
        std::thread::Builder::new()
            .name("rtng-handle-sweep".to_string())
            .spawn(move || loop {
                std::thread::sleep(SWEEP_INTERVAL);
                let closed = sweep_handles.sweep_idle();
                if closed > 0 {
                    tracing::debug!(closed, "idle file handles closed");
                }
            })
            .expect("spawn handle sweeper");

        tracing::info!(
            handle_cap = cap,
            frame_cap_mb,
            idle_secs,
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

    /// Frame-pool hard cap.
    pub fn frame_cap_bytes(&self) -> u64 {
        self.frames.cap_bytes()
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
            Ok(Err(e)) => Err(StorageError::io(path.display().to_string(), e)),
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
        match self.backend.pwrite(file, data, offset).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(StorageError::io(path.display().to_string(), e)),
            Err(_) => Err(StorageError::Cancelled),
        }
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
}
