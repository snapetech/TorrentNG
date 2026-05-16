use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::instrument;

use rt_path::{StorageProfile, StorageRootId};

use crate::{error::StorageError, io_class::IoClass};

/// A pending read or write operation against a storage root.
#[derive(Debug)]
pub struct IoRequest {
    pub class: IoClass,
    pub storage_root: StorageRootId,
    pub file_path: PathBuf,
    pub offset: u64,
    pub len: usize,
}

/// Per-mount concurrency configuration.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub profile: StorageProfile,
    /// Maximum queue depth across all classes (backpressure point).
    pub max_queue: usize,
    /// Override per-class concurrency limits (0 = use profile default).
    pub recheck_concurrency: usize,
    pub peer_read_concurrency: usize,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        SchedulerConfig {
            profile: StorageProfile::Unknown,
            max_queue: 256,
            recheck_concurrency: 0,
            peer_read_concurrency: 0,
        }
    }
}

/// Per-mount I/O scheduler.
///
/// Enforces class-based concurrency limits so that peer reads are never
/// starved by recheck or move/copy operations.
///
/// Phase 2 implements the semaphore-per-class model. A full priority queue
/// with preemption is deferred to Phase 9 (scale hardening).
#[derive(Debug, Clone)]
pub struct MountScheduler {
    storage_root: StorageRootId,
    recheck_sem: Arc<Semaphore>,
    move_copy_sem: Arc<Semaphore>,
    peer_write_sem: Arc<Semaphore>,
    peer_read_sem: Arc<Semaphore>,
    foreground_sem: Arc<Semaphore>,
    metadata_sem: Arc<Semaphore>,
}

impl MountScheduler {
    pub fn new(storage_root: StorageRootId, config: &SchedulerConfig) -> Self {
        let ssd = matches!(config.profile, StorageProfile::Ssd | StorageProfile::Nvme);

        let recheck_limit = if config.recheck_concurrency > 0 {
            config.recheck_concurrency
        } else if ssd {
            IoClass::Recheck.ssd_concurrency()
        } else {
            IoClass::Recheck.hdd_concurrency()
        };

        let peer_read_limit = if config.peer_read_concurrency > 0 {
            config.peer_read_concurrency
        } else if ssd {
            IoClass::PeerRead.ssd_concurrency()
        } else {
            IoClass::PeerRead.hdd_concurrency()
        };

        let mv = if ssd {
            IoClass::MoveCopy.ssd_concurrency()
        } else {
            IoClass::MoveCopy.hdd_concurrency()
        };
        let pw = if ssd {
            IoClass::PeerWrite.ssd_concurrency()
        } else {
            IoClass::PeerWrite.hdd_concurrency()
        };
        let fg = if ssd {
            IoClass::Foreground.ssd_concurrency()
        } else {
            IoClass::Foreground.hdd_concurrency()
        };
        let md = if ssd {
            IoClass::Metadata.ssd_concurrency()
        } else {
            IoClass::Metadata.hdd_concurrency()
        };

        MountScheduler {
            storage_root,
            recheck_sem: Arc::new(Semaphore::new(recheck_limit)),
            move_copy_sem: Arc::new(Semaphore::new(mv)),
            peer_write_sem: Arc::new(Semaphore::new(pw)),
            peer_read_sem: Arc::new(Semaphore::new(peer_read_limit)),
            foreground_sem: Arc::new(Semaphore::new(fg)),
            metadata_sem: Arc::new(Semaphore::new(md)),
        }
    }

    pub fn storage_root(&self) -> StorageRootId {
        self.storage_root
    }

    /// Acquire a concurrency permit for the given I/O class.
    ///
    /// The caller holds the permit for the duration of the I/O operation,
    /// then drops it to allow the next waiter to proceed.
    #[instrument(skip(self), fields(class = ?class))]
    pub async fn acquire(&self, class: IoClass) -> Result<OwnedSemaphorePermit, StorageError> {
        let sem = match class {
            IoClass::Recheck => &self.recheck_sem,
            IoClass::MoveCopy => &self.move_copy_sem,
            IoClass::PeerWrite => &self.peer_write_sem,
            IoClass::PeerRead => &self.peer_read_sem,
            IoClass::Foreground => &self.foreground_sem,
            IoClass::Metadata => &self.metadata_sem,
        };
        sem.clone()
            .acquire_owned()
            .await
            .map_err(|_| StorageError::Cancelled)
    }

    /// Try to acquire a permit without waiting. Returns None if at capacity.
    pub fn try_acquire(&self, class: IoClass) -> Option<OwnedSemaphorePermit> {
        let sem = match class {
            IoClass::Recheck => &self.recheck_sem,
            IoClass::MoveCopy => &self.move_copy_sem,
            IoClass::PeerWrite => &self.peer_write_sem,
            IoClass::PeerRead => &self.peer_read_sem,
            IoClass::Foreground => &self.foreground_sem,
            IoClass::Metadata => &self.metadata_sem,
        };
        sem.clone().try_acquire_owned().ok()
    }

    /// Available permits for a given class (for metrics/diagnostics).
    pub fn available_permits(&self, class: IoClass) -> usize {
        match class {
            IoClass::Recheck => self.recheck_sem.available_permits(),
            IoClass::MoveCopy => self.move_copy_sem.available_permits(),
            IoClass::PeerWrite => self.peer_write_sem.available_permits(),
            IoClass::PeerRead => self.peer_read_sem.available_permits(),
            IoClass::Foreground => self.foreground_sem.available_permits(),
            IoClass::Metadata => self.metadata_sem.available_permits(),
        }
    }
}

/// Read bytes from a file, respecting the scheduler permit.
///
/// Opens the file in read-only mode — never creates or truncates.
#[instrument(skip(scheduler), fields(path = %path.display(), offset, len))]
pub async fn scheduled_read(
    scheduler: &MountScheduler,
    class: IoClass,
    path: &std::path::Path,
    offset: u64,
    len: usize,
) -> Result<bytes::Bytes, StorageError> {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncSeekExt;

    let _permit = scheduler.acquire(class).await?;

    let path_str = path.display().to_string();
    let mut file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(false)
        .create(false) // never create
        .truncate(false) // never truncate
        .open(path)
        .await
        .map_err(|e| StorageError::io(&path_str, e))?;

    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|e| StorageError::io(&path_str, e))?;

    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf)
        .await
        .map_err(|e| StorageError::io(&path_str, e))?;

    tracing::debug!(path = %path_str, offset, len, "read complete");
    Ok(bytes::Bytes::from(buf))
}

/// Write bytes to a file at an offset (for piece writes during download).
///
/// Opens existing file for writing only — never creates new files unless
/// `create` is true (only for initial allocation during import).
#[instrument(skip(scheduler, data), fields(path = %path.display(), offset, len = data.len()))]
pub async fn scheduled_write(
    scheduler: &MountScheduler,
    class: IoClass,
    path: &std::path::Path,
    offset: u64,
    data: bytes::Bytes,
    create: bool,
) -> Result<(), StorageError> {
    use tokio::io::AsyncSeekExt;
    use tokio::io::AsyncWriteExt;

    let _permit = scheduler.acquire(class).await?;

    let path_str = path.display().to_string();
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).read(false).truncate(false);
    if create {
        opts.create(true);
    } else {
        opts.create(false);
    }

    let mut file = opts
        .open(path)
        .await
        .map_err(|e| StorageError::io(&path_str, e))?;

    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|e| StorageError::io(&path_str, e))?;

    file.write_all(&data)
        .await
        .map_err(|e| StorageError::io(&path_str, e))?;

    tracing::debug!(path = %path_str, offset, len = data.len(), "write complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rt_path::StorageRootId;

    fn hdd_scheduler() -> MountScheduler {
        MountScheduler::new(
            StorageRootId::new(),
            &SchedulerConfig {
                profile: StorageProfile::Hdd,
                ..Default::default()
            },
        )
    }

    fn ssd_scheduler() -> MountScheduler {
        MountScheduler::new(
            StorageRootId::new(),
            &SchedulerConfig {
                profile: StorageProfile::Ssd,
                ..Default::default()
            },
        )
    }

    #[tokio::test]
    async fn acquire_and_release_recheck() {
        let sched = hdd_scheduler();
        // HDD recheck limit = 1
        let permit = sched.acquire(IoClass::Recheck).await.unwrap();
        assert_eq!(sched.available_permits(IoClass::Recheck), 0);
        drop(permit);
        assert_eq!(sched.available_permits(IoClass::Recheck), 1);
    }

    #[tokio::test]
    async fn peer_read_not_starved_by_recheck() {
        let sched = hdd_scheduler();
        // Hold all recheck slots
        let _r = sched.acquire(IoClass::Recheck).await.unwrap();
        // Peer read should still be acquirable (separate semaphore)
        let permit = sched.try_acquire(IoClass::PeerRead);
        assert!(permit.is_some());
    }

    #[test]
    fn ssd_has_higher_concurrency() {
        let hdd = hdd_scheduler();
        let ssd = ssd_scheduler();
        assert!(
            ssd.available_permits(IoClass::PeerRead) > hdd.available_permits(IoClass::PeerRead)
        );
    }

    #[tokio::test]
    async fn read_nonexistent_file() {
        let sched = hdd_scheduler();
        let result = scheduled_read(
            &sched,
            IoClass::Foreground,
            std::path::Path::new("/nonexistent/path/file.bin"),
            0,
            16,
        )
        .await;
        assert!(matches!(result, Err(StorageError::FileNotFound { .. })));
    }

    #[tokio::test]
    async fn read_and_write_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        // Pre-create file with known content
        std::fs::write(&path, b"hello world!").unwrap();

        let sched = ssd_scheduler();

        // Read it
        let data = scheduled_read(&sched, IoClass::Foreground, &path, 6, 5)
            .await
            .unwrap();
        assert_eq!(&data[..], b"world");
    }

    #[tokio::test]
    async fn write_does_not_create_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist.bin");
        let sched = hdd_scheduler();

        let result = scheduled_write(
            &sched,
            IoClass::PeerWrite,
            &path,
            0,
            bytes::Bytes::from_static(b"data"),
            false, // create = false
        )
        .await;
        // Should fail because the file doesn't exist and create=false
        assert!(result.is_err());
        assert!(!path.exists());
    }
}
