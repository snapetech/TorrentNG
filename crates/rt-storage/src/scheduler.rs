use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};
use tracing::instrument;

use rt_hash::{merkle_root, BlockHash};
use rt_path::{StorageProfile, StorageRootId};
use sha1::{Digest, Sha1};

use crate::{error::StorageError, io_class::IoClass};

#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;

/// A pending read or write operation against a storage root.
#[derive(Debug)]
pub struct IoRequest {
    pub class: IoClass,
    pub storage_root: StorageRootId,
    pub file_path: PathBuf,
    pub offset: u64,
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreallocationMode {
    Off,
    Sparse,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurabilityMode {
    Fast,
    Checkpoint,
    Strict,
}

#[derive(Debug, Clone)]
pub struct StorageIoConfig {
    pub file_pool_size: usize,
    pub idle_file_ttl_secs: u64,
    pub io_worker_threads: usize,
    pub io_queue_depth: usize,
    pub hash_worker_threads: usize,
    pub hash_queue_depth: usize,
    pub preallocation_mode: PreallocationMode,
    pub durability_mode: DurabilityMode,
    pub peer_read_readahead_bytes: usize,
}

impl Default for StorageIoConfig {
    fn default() -> Self {
        Self {
            file_pool_size: 512,
            idle_file_ttl_secs: 300,
            io_worker_threads: 4,
            io_queue_depth: 256,
            hash_worker_threads: 2,
            hash_queue_depth: 256,
            preallocation_mode: PreallocationMode::Sparse,
            durability_mode: DurabilityMode::Checkpoint,
            peer_read_readahead_bytes: 512 * 1024,
        }
    }
}

/// Per-mount concurrency configuration.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub profile: StorageProfile,
    /// Maximum queue depth across all classes (backpressure point).
    pub max_queue: usize,
    pub recheck_concurrency: usize,
    pub peer_read_concurrency: usize,
    pub storage_io: StorageIoConfig,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        SchedulerConfig {
            profile: StorageProfile::Unknown,
            max_queue: 256,
            recheck_concurrency: 0,
            peer_read_concurrency: 0,
            storage_io: StorageIoConfig::default(),
        }
    }
}

#[derive(Debug, Default)]
pub struct FilePoolStats {
    pub capacity: usize,
    pub open_files: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub idle_closes: u64,
}

#[derive(Debug, Default)]
pub struct StorageIoStats {
    pub file_pool: FilePoolStats,
    pub io_queue_depth: usize,
    pub hash_queue_depth: usize,
    pub dirty_files: usize,
    pub read_ops_by_class: [u64; 6],
    pub write_ops_by_class: [u64; 6],
    pub bytes_read_by_class: [u64; 6],
    pub bytes_written_by_class: [u64; 6],
    pub sync_ops: u64,
    pub hash_ops: u64,
    pub preallocation_failures: u64,
    pub preallocation_fallbacks: u64,
    pub peer_read_cache_entries: usize,
    pub peer_read_cache_hits: u64,
    pub peer_read_cache_misses: u64,
}

#[derive(Debug, Default)]
struct FilePoolCounters {
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
    idle_closes: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenMode {
    Read,
    Write,
}

#[derive(Debug)]
struct CachedFile {
    file: Arc<File>,
    mode: OpenMode,
    last_used: Instant,
    sequence: u64,
}

#[derive(Debug)]
struct FilePool {
    capacity: usize,
    idle_ttl: Duration,
    entries: Mutex<HashMap<PathBuf, CachedFile>>,
    counters: FilePoolCounters,
    sequence: AtomicU64,
}

impl FilePool {
    fn new(capacity: usize, idle_ttl: Duration) -> Self {
        Self {
            capacity: capacity.max(1),
            idle_ttl,
            entries: Mutex::new(HashMap::new()),
            counters: FilePoolCounters::default(),
            sequence: AtomicU64::new(1),
        }
    }

    fn get_or_open(
        &self,
        path: &Path,
        mode: OpenMode,
        create: bool,
    ) -> Result<Arc<File>, StorageError> {
        let key = normalized_key(path);
        let path_str = key.display().to_string();
        let now = Instant::now();
        let seq = self.sequence.fetch_add(1, Ordering::Relaxed);

        let mut entries = self.entries.lock().expect("file pool mutex poisoned");
        self.sweep_idle_locked(&mut entries, now);
        if let Some(entry) = entries.get_mut(&key) {
            if entry.mode == mode {
                entry.last_used = now;
                entry.sequence = seq;
                self.counters.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(entry.file.clone());
            }
            entries.remove(&key);
            self.counters.evictions.fetch_add(1, Ordering::Relaxed);
        }

        self.counters.misses.fetch_add(1, Ordering::Relaxed);
        let mut opts = OpenOptions::new();
        match mode {
            OpenMode::Read => {
                opts.read(true).write(false).create(false).truncate(false);
            }
            OpenMode::Write => {
                opts.read(false).write(true).create(create).truncate(false);
            }
        }
        let file = Arc::new(
            opts.open(&key)
                .map_err(|e| StorageError::io(&path_str, e))?,
        );
        entries.insert(
            key,
            CachedFile {
                file: file.clone(),
                mode,
                last_used: now,
                sequence: seq,
            },
        );
        self.enforce_capacity_locked(&mut entries);
        Ok(file)
    }

    fn sync_path(&self, path: &Path) -> Result<(), StorageError> {
        let key = normalized_key(path);
        let path_str = key.display().to_string();
        let file = {
            let entries = self.entries.lock().expect("file pool mutex poisoned");
            entries.get(&key).map(|entry| entry.file.clone())
        };
        let file = match file {
            Some(file) => file,
            None => Arc::new(
                OpenOptions::new()
                    .read(false)
                    .write(true)
                    .create(false)
                    .truncate(false)
                    .open(&key)
                    .map_err(|e| StorageError::io(&path_str, e))?,
            ),
        };
        file.sync_data().map_err(|e| StorageError::io(path_str, e))
    }

    fn sync_all(&self) -> Result<(), StorageError> {
        let files: Vec<(PathBuf, Arc<File>)> = {
            let entries = self.entries.lock().expect("file pool mutex poisoned");
            entries
                .iter()
                .filter(|(_, entry)| entry.mode == OpenMode::Write)
                .map(|(path, entry)| (path.clone(), entry.file.clone()))
                .collect()
        };
        for (path, file) in files {
            let path_str = path.display().to_string();
            file.sync_data()
                .map_err(|e| StorageError::io(path_str, e))?;
        }
        Ok(())
    }

    fn stats(&self) -> FilePoolStats {
        let entries = self.entries.lock().expect("file pool mutex poisoned");
        FilePoolStats {
            capacity: self.capacity,
            open_files: entries.len(),
            hits: self.counters.hits.load(Ordering::Relaxed),
            misses: self.counters.misses.load(Ordering::Relaxed),
            evictions: self.counters.evictions.load(Ordering::Relaxed),
            idle_closes: self.counters.idle_closes.load(Ordering::Relaxed),
        }
    }

    fn sweep_idle_locked(&self, entries: &mut HashMap<PathBuf, CachedFile>, now: Instant) {
        if self.idle_ttl.is_zero() {
            return;
        }
        let before = entries.len();
        entries.retain(|_, entry| now.duration_since(entry.last_used) < self.idle_ttl);
        let closed = before.saturating_sub(entries.len()) as u64;
        if closed > 0 {
            self.counters
                .idle_closes
                .fetch_add(closed, Ordering::Relaxed);
        }
    }

    fn enforce_capacity_locked(&self, entries: &mut HashMap<PathBuf, CachedFile>) {
        while entries.len() > self.capacity {
            let Some(path) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.sequence)
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            entries.remove(&path);
            self.counters.evictions.fetch_add(1, Ordering::Relaxed);
        }
    }
}

type BlockingJob = Box<dyn FnOnce() + Send + 'static>;

#[derive(Debug)]
struct BlockingPool {
    sender: mpsc::SyncSender<BlockingJob>,
    queued: Arc<AtomicUsize>,
}

impl BlockingPool {
    fn new(name_prefix: &'static str, worker_threads: usize, queue_depth: usize) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<BlockingJob>(queue_depth.max(1));
        let receiver = Arc::new(Mutex::new(receiver));
        let queued = Arc::new(AtomicUsize::new(0));
        for index in 0..worker_threads.max(1) {
            let receiver = receiver.clone();
            let queued = queued.clone();
            let name = format!("{name_prefix}-{index}");
            std::thread::Builder::new()
                .name(name)
                .spawn(move || loop {
                    let job = {
                        let rx = receiver.lock().expect("I/O pool mutex poisoned");
                        rx.recv()
                    };
                    match job {
                        Ok(job) => {
                            queued.fetch_sub(1, Ordering::Relaxed);
                            job();
                        }
                        Err(_) => break,
                    }
                })
                .expect("failed to spawn storage I/O worker");
        }
        Self { sender, queued }
    }

    async fn run<T, F>(&self, f: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, StorageError> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.queued.fetch_add(1, Ordering::Relaxed);
        if self
            .sender
            .send(Box::new(move || {
                let _ = tx.send(f());
            }))
            .is_err()
        {
            self.queued.fetch_sub(1, Ordering::Relaxed);
            return Err(StorageError::Cancelled);
        }
        rx.await.map_err(|_| StorageError::Cancelled)?
    }

    fn queued(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }
}

/// Per-mount I/O scheduler.
#[derive(Debug, Clone)]
pub struct MountScheduler {
    storage_root: StorageRootId,
    recheck_sem: Arc<Semaphore>,
    move_copy_sem: Arc<Semaphore>,
    peer_write_sem: Arc<Semaphore>,
    peer_read_sem: Arc<Semaphore>,
    foreground_sem: Arc<Semaphore>,
    metadata_sem: Arc<Semaphore>,
    queue_sem: Arc<Semaphore>,
    io_config: StorageIoConfig,
    file_pool: Arc<FilePool>,
    io_pool: Arc<BlockingPool>,
    hash_pool: Arc<BlockingPool>,
    dirty_paths: Arc<Mutex<HashSet<PathBuf>>>,
    peer_read_cache: Arc<Mutex<HashMap<PathBuf, PeerReadCacheEntry>>>,
    counters: Arc<StorageCounters>,
}

#[derive(Debug)]
struct StorageCounters {
    read_ops_by_class: [AtomicU64; 6],
    write_ops_by_class: [AtomicU64; 6],
    bytes_read_by_class: [AtomicU64; 6],
    bytes_written_by_class: [AtomicU64; 6],
    sync_ops: AtomicU64,
    hash_ops: AtomicU64,
    preallocation_failures: AtomicU64,
    preallocation_fallbacks: AtomicU64,
    peer_read_cache_hits: AtomicU64,
    peer_read_cache_misses: AtomicU64,
}

impl Default for StorageCounters {
    fn default() -> Self {
        Self {
            read_ops_by_class: std::array::from_fn(|_| AtomicU64::new(0)),
            write_ops_by_class: std::array::from_fn(|_| AtomicU64::new(0)),
            bytes_read_by_class: std::array::from_fn(|_| AtomicU64::new(0)),
            bytes_written_by_class: std::array::from_fn(|_| AtomicU64::new(0)),
            sync_ops: AtomicU64::new(0),
            hash_ops: AtomicU64::new(0),
            preallocation_failures: AtomicU64::new(0),
            preallocation_fallbacks: AtomicU64::new(0),
            peer_read_cache_hits: AtomicU64::new(0),
            peer_read_cache_misses: AtomicU64::new(0),
        }
    }
}

#[derive(Debug)]
struct PeerReadCacheEntry {
    offset: u64,
    data: bytes::Bytes,
    last_used: Instant,
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
        let io_config = config.storage_io.clone();

        MountScheduler {
            storage_root,
            recheck_sem: Arc::new(Semaphore::new(recheck_limit)),
            move_copy_sem: Arc::new(Semaphore::new(mv)),
            peer_write_sem: Arc::new(Semaphore::new(pw)),
            peer_read_sem: Arc::new(Semaphore::new(peer_read_limit)),
            foreground_sem: Arc::new(Semaphore::new(fg)),
            metadata_sem: Arc::new(Semaphore::new(md)),
            queue_sem: Arc::new(Semaphore::new(
                config.max_queue.min(io_config.io_queue_depth).max(1),
            )),
            file_pool: Arc::new(FilePool::new(
                clamp_file_pool_size(io_config.file_pool_size),
                Duration::from_secs(io_config.idle_file_ttl_secs),
            )),
            io_pool: Arc::new(BlockingPool::new(
                "rt-storage-io",
                io_config.io_worker_threads,
                io_config.io_queue_depth,
            )),
            hash_pool: Arc::new(BlockingPool::new(
                "rt-storage-hash",
                io_config.hash_worker_threads,
                io_config.hash_queue_depth,
            )),
            dirty_paths: Arc::new(Mutex::new(HashSet::new())),
            peer_read_cache: Arc::new(Mutex::new(HashMap::new())),
            counters: Arc::new(StorageCounters::default()),
            io_config,
        }
    }

    pub fn storage_root(&self) -> StorageRootId {
        self.storage_root
    }

    pub fn io_config(&self) -> &StorageIoConfig {
        &self.io_config
    }

    pub fn file_pool_stats(&self) -> FilePoolStats {
        self.file_pool.stats()
    }

    pub fn io_queue_depth(&self) -> usize {
        self.io_pool.queued()
    }

    pub fn stats(&self) -> StorageIoStats {
        let dirty_files = self
            .dirty_paths
            .lock()
            .expect("dirty path mutex poisoned")
            .len();
        let peer_read_cache_entries = self
            .peer_read_cache
            .lock()
            .expect("peer read cache mutex poisoned")
            .len();
        StorageIoStats {
            file_pool: self.file_pool.stats(),
            io_queue_depth: self.io_pool.queued(),
            hash_queue_depth: self.hash_pool.queued(),
            dirty_files,
            read_ops_by_class: load_atomic_array(&self.counters.read_ops_by_class),
            write_ops_by_class: load_atomic_array(&self.counters.write_ops_by_class),
            bytes_read_by_class: load_atomic_array(&self.counters.bytes_read_by_class),
            bytes_written_by_class: load_atomic_array(&self.counters.bytes_written_by_class),
            sync_ops: self.counters.sync_ops.load(Ordering::Relaxed),
            hash_ops: self.counters.hash_ops.load(Ordering::Relaxed),
            preallocation_failures: self.counters.preallocation_failures.load(Ordering::Relaxed),
            preallocation_fallbacks: self
                .counters
                .preallocation_fallbacks
                .load(Ordering::Relaxed),
            peer_read_cache_entries,
            peer_read_cache_hits: self.counters.peer_read_cache_hits.load(Ordering::Relaxed),
            peer_read_cache_misses: self.counters.peer_read_cache_misses.load(Ordering::Relaxed),
        }
    }

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

    async fn submit<T, F>(&self, f: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, StorageError> + Send + 'static,
    {
        let _queue = self
            .queue_sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| StorageError::Cancelled)?;
        self.io_pool.run(f).await
    }

    pub async fn read_at(
        &self,
        class: IoClass,
        path: &Path,
        offset: u64,
        len: usize,
    ) -> Result<bytes::Bytes, StorageError> {
        let _permit = self.acquire(class).await?;
        let pool = self.file_pool.clone();
        let counters = self.counters.clone();
        let peer_read_cache = self.peer_read_cache.clone();
        let readahead_bytes = self.io_config.peer_read_readahead_bytes;
        let path = path.to_path_buf();
        self.submit(move || {
            let key = normalized_key(&path);
            let path_str = key.display().to_string();
            if class == IoClass::PeerRead && readahead_bytes > len {
                if let Some(bytes) = peer_read_cache_hit(&peer_read_cache, &key, offset, len) {
                    counters
                        .peer_read_cache_hits
                        .fetch_add(1, Ordering::Relaxed);
                    counters.read_ops_by_class[class_index(class)].fetch_add(1, Ordering::Relaxed);
                    counters.bytes_read_by_class[class_index(class)]
                        .fetch_add(len as u64, Ordering::Relaxed);
                    return Ok(bytes);
                }
                counters
                    .peer_read_cache_misses
                    .fetch_add(1, Ordering::Relaxed);
            }
            let file = pool.get_or_open(&key, OpenMode::Read, false)?;
            let read_len = if class == IoClass::PeerRead && readahead_bytes > len {
                let file_len = file
                    .metadata()
                    .map_err(|e| StorageError::io(&path_str, e))?
                    .len();
                if offset >= file_len {
                    len
                } else {
                    readahead_bytes
                        .min(file_len.saturating_sub(offset) as usize)
                        .max(len)
                }
            } else {
                len
            };
            let mut buf = vec![0u8; read_len];
            let mut read = 0usize;
            while read < read_len {
                let n = positioned_read(&file, &mut buf[read..], offset + read as u64)
                    .map_err(|e| StorageError::io(&path_str, e))?;
                if n == 0 {
                    return Err(StorageError::ShortIo {
                        path: path_str,
                        expected: read_len,
                        actual: read,
                    });
                }
                read += n;
            }
            counters.read_ops_by_class[class_index(class)].fetch_add(1, Ordering::Relaxed);
            counters.bytes_read_by_class[class_index(class)]
                .fetch_add(read as u64, Ordering::Relaxed);
            let bytes = bytes::Bytes::from(buf);
            if class == IoClass::PeerRead && read_len > len {
                let exact = bytes.slice(..len);
                peer_read_cache_store(&peer_read_cache, key, offset, bytes);
                Ok(exact)
            } else {
                Ok(bytes)
            }
        })
        .await
    }

    pub async fn write_at(
        &self,
        class: IoClass,
        path: &Path,
        offset: u64,
        data: bytes::Bytes,
        create: bool,
    ) -> Result<(), StorageError> {
        let _permit = self.acquire(class).await?;
        let strict = self.io_config.durability_mode == DurabilityMode::Strict;
        let pool = self.file_pool.clone();
        let dirty_paths = self.dirty_paths.clone();
        let counters = self.counters.clone();
        let path = path.to_path_buf();
        self.submit(move || {
            let key = normalized_key(&path);
            let path_str = key.display().to_string();
            let file = pool.get_or_open(&key, OpenMode::Write, create)?;
            let mut written = 0usize;
            while written < data.len() {
                let n = positioned_write(&file, &data[written..], offset + written as u64)
                    .map_err(|e| StorageError::io(&path_str, e))?;
                if n == 0 {
                    return Err(StorageError::ShortIo {
                        path: path_str,
                        expected: data.len(),
                        actual: written,
                    });
                }
                written += n;
            }
            if strict {
                file.sync_data()
                    .map_err(|e| StorageError::io(&path_str, e))?;
            } else {
                let mut dirty = dirty_paths.lock().expect("dirty path mutex poisoned");
                dirty.insert(key);
            }
            counters.write_ops_by_class[class_index(class)].fetch_add(1, Ordering::Relaxed);
            counters.bytes_written_by_class[class_index(class)]
                .fetch_add(written as u64, Ordering::Relaxed);
            Ok(())
        })
        .await
    }

    pub async fn prepare_file(
        &self,
        path: &Path,
        len: u64,
        mode: PreallocationMode,
    ) -> Result<(), StorageError> {
        let pool = self.file_pool.clone();
        let dirty_paths = self.dirty_paths.clone();
        let counters = self.counters.clone();
        let path = path.to_path_buf();
        self.submit(move || {
            let key = normalized_key(&path);
            let path_str = key.display().to_string();
            if let Some(parent) = key.parent() {
                std::fs::create_dir_all(parent).map_err(|e| StorageError::io(parent.display().to_string(), e))?;
            }
            let file = pool.get_or_open(&key, OpenMode::Write, true)?;
            match mode {
                PreallocationMode::Off => {}
                PreallocationMode::Sparse => {
                    file.set_len(len).map_err(|e| StorageError::io(&path_str, e))?;
                }
                PreallocationMode::Full => {
                    if let Err(e) = full_preallocate(&file, len) {
                        counters
                            .preallocation_fallbacks
                            .fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(path = %path_str, err = %e, "full preallocation failed; falling back to sparse length");
                        file.set_len(len).map_err(|fallback| {
                            counters
                                .preallocation_failures
                                .fetch_add(1, Ordering::Relaxed);
                            StorageError::io(&path_str, fallback)
                        })?;
                    }
                }
            }
            if mode != PreallocationMode::Off {
                let mut dirty = dirty_paths.lock().expect("dirty path mutex poisoned");
                dirty.insert(key);
            }
            Ok(())
        })
        .await
    }

    pub async fn sync_data(&self, path: &Path) -> Result<(), StorageError> {
        let pool = self.file_pool.clone();
        let dirty_paths = self.dirty_paths.clone();
        let counters = self.counters.clone();
        let path = path.to_path_buf();
        self.submit(move || {
            let key = normalized_key(&path);
            pool.sync_path(&key)?;
            counters.sync_ops.fetch_add(1, Ordering::Relaxed);
            let mut dirty = dirty_paths.lock().expect("dirty path mutex poisoned");
            dirty.remove(&key);
            Ok(())
        })
        .await
    }

    pub async fn sync_all_open_files(&self) -> Result<(), StorageError> {
        let pool = self.file_pool.clone();
        let dirty_paths = self.dirty_paths.clone();
        let counters = self.counters.clone();
        self.submit(move || {
            pool.sync_all()?;
            counters.sync_ops.fetch_add(1, Ordering::Relaxed);
            let paths: Vec<PathBuf> = {
                let dirty = dirty_paths.lock().expect("dirty path mutex poisoned");
                dirty.iter().cloned().collect()
            };
            for path in &paths {
                pool.sync_path(path)?;
            }
            let mut dirty = dirty_paths.lock().expect("dirty path mutex poisoned");
            for path in paths {
                dirty.remove(&path);
            }
            Ok(())
        })
        .await
    }

    pub async fn hash_sha1(&self, data: bytes::Bytes) -> Result<[u8; 20], StorageError> {
        let counters = self.counters.clone();
        self.hash_pool
            .run(move || {
                let mut hasher = Sha1::new();
                hasher.update(&data);
                counters.hash_ops.fetch_add(1, Ordering::Relaxed);
                Ok(hasher.finalize().into())
            })
            .await
    }

    pub async fn hash_v2_leaf(&self, data: bytes::Bytes) -> Result<[u8; 32], StorageError> {
        let counters = self.counters.clone();
        self.hash_pool
            .run(move || {
                counters.hash_ops.fetch_add(1, Ordering::Relaxed);
                Ok(BlockHash::of(&data).0)
            })
            .await
    }

    pub async fn hash_v2_root(&self, leaves: Vec<[u8; 32]>) -> Result<[u8; 32], StorageError> {
        let counters = self.counters.clone();
        self.hash_pool
            .run(move || {
                counters.hash_ops.fetch_add(1, Ordering::Relaxed);
                Ok(merkle_root(&leaves))
            })
            .await
    }
}

#[instrument(skip(scheduler), fields(path = %path.display(), offset, len))]
pub async fn scheduled_read(
    scheduler: &MountScheduler,
    class: IoClass,
    path: &Path,
    offset: u64,
    len: usize,
) -> Result<bytes::Bytes, StorageError> {
    scheduler.read_at(class, path, offset, len).await
}

#[instrument(skip(scheduler, data), fields(path = %path.display(), offset, len = data.len()))]
pub async fn scheduled_write(
    scheduler: &MountScheduler,
    class: IoClass,
    path: &Path,
    offset: u64,
    data: bytes::Bytes,
    create: bool,
) -> Result<(), StorageError> {
    scheduler.write_at(class, path, offset, data, create).await
}

fn normalized_key(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn class_index(class: IoClass) -> usize {
    class as usize
}

fn load_atomic_array(values: &[AtomicU64; 6]) -> [u64; 6] {
    std::array::from_fn(|index| values[index].load(Ordering::Relaxed))
}

fn peer_read_cache_hit(
    cache: &Mutex<HashMap<PathBuf, PeerReadCacheEntry>>,
    key: &Path,
    offset: u64,
    len: usize,
) -> Option<bytes::Bytes> {
    let mut cache = cache.lock().expect("peer read cache mutex poisoned");
    let entry = cache.get_mut(key)?;
    let relative = offset.checked_sub(entry.offset)? as usize;
    let end = relative.checked_add(len)?;
    if end <= entry.data.len() {
        entry.last_used = Instant::now();
        Some(entry.data.slice(relative..end))
    } else {
        None
    }
}

fn peer_read_cache_store(
    cache: &Mutex<HashMap<PathBuf, PeerReadCacheEntry>>,
    key: PathBuf,
    offset: u64,
    data: bytes::Bytes,
) {
    const MAX_PEER_READ_CACHE_ENTRIES: usize = 64;
    let mut cache = cache.lock().expect("peer read cache mutex poisoned");
    if cache.len() >= MAX_PEER_READ_CACHE_ENTRIES {
        if let Some(evict) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&evict);
        }
    }
    cache.insert(
        key,
        PeerReadCacheEntry {
            offset,
            data,
            last_used: Instant::now(),
        },
    );
}

fn clamp_file_pool_size(configured: usize) -> usize {
    let configured = configured.max(1);
    #[cfg(unix)]
    {
        let mut limits = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) };
        if rc == 0 && limits.rlim_cur != libc::RLIM_INFINITY {
            let soft = limits.rlim_cur as usize;
            let budget = soft.saturating_mul(3) / 4;
            return configured.min(budget.max(1));
        }
    }
    configured
}

#[cfg(unix)]
fn positioned_read(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    file.read_at(buf, offset)
}

#[cfg(windows)]
fn positioned_read(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    file.seek_read(buf, offset)
}

#[cfg(unix)]
fn positioned_write(file: &File, buf: &[u8], offset: u64) -> std::io::Result<usize> {
    file.write_at(buf, offset)
}

#[cfg(windows)]
fn positioned_write(file: &File, buf: &[u8], offset: u64) -> std::io::Result<usize> {
    file.seek_write(buf, offset)
}

fn full_preallocate(file: &File, len: u64) -> Result<(), StorageError> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let rc = unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, len as libc::off_t) };
        if rc == 0 {
            return Ok(());
        }
        return Err(StorageError::Io {
            path: "<preallocate>".to_owned(),
            source: std::io::Error::from_raw_os_error(rc),
        });
    }
    #[cfg(not(target_os = "linux"))]
    {
        file.set_len(len)
            .map_err(|e| StorageError::io("<preallocate>", e))
    }
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
                storage_io: StorageIoConfig {
                    file_pool_size: 2,
                    idle_file_ttl_secs: 3600,
                    io_worker_threads: 2,
                    io_queue_depth: 8,
                    ..Default::default()
                },
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
        let permit = sched.acquire(IoClass::Recheck).await.unwrap();
        assert_eq!(sched.available_permits(IoClass::Recheck), 0);
        drop(permit);
        assert_eq!(sched.available_permits(IoClass::Recheck), 1);
    }

    #[tokio::test]
    async fn peer_read_not_starved_by_recheck() {
        let sched = hdd_scheduler();
        let _r = sched.acquire(IoClass::Recheck).await.unwrap();
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
    async fn read_nonexistent_file_does_not_create() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.bin");
        let sched = hdd_scheduler();
        let result = scheduled_read(&sched, IoClass::Foreground, &path, 0, 16).await;
        assert!(matches!(result, Err(StorageError::FileNotFound { .. })));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn read_and_write_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        std::fs::write(&path, b"hello world!").unwrap();
        let sched = ssd_scheduler();
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
            false,
        )
        .await;
        assert!(result.is_err());
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn file_pool_records_hits_and_evictions() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        let c = dir.path().join("c.bin");
        std::fs::write(&a, b"aaaa").unwrap();
        std::fs::write(&b, b"bbbb").unwrap();
        std::fs::write(&c, b"cccc").unwrap();
        let sched = hdd_scheduler();

        scheduled_read(&sched, IoClass::Foreground, &a, 0, 1)
            .await
            .unwrap();
        scheduled_read(&sched, IoClass::Foreground, &a, 1, 1)
            .await
            .unwrap();
        scheduled_read(&sched, IoClass::Foreground, &b, 0, 1)
            .await
            .unwrap();
        scheduled_read(&sched, IoClass::Foreground, &c, 0, 1)
            .await
            .unwrap();

        let stats = sched.file_pool_stats();
        assert!(stats.hits >= 1);
        assert!(stats.misses >= 3);
        assert!(stats.evictions >= 1);
        assert_eq!(stats.open_files, 2);
    }

    #[tokio::test]
    async fn concurrent_positioned_writes_do_not_share_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("positioned.bin");
        let sched = hdd_scheduler();
        sched
            .prepare_file(&path, 8, PreallocationMode::Sparse)
            .await
            .unwrap();
        let a = scheduled_write(
            &sched,
            IoClass::PeerWrite,
            &path,
            0,
            bytes::Bytes::from_static(b"aaaa"),
            false,
        );
        let b = scheduled_write(
            &sched,
            IoClass::PeerWrite,
            &path,
            4,
            bytes::Bytes::from_static(b"bbbb"),
            false,
        );
        let (ra, rb) = tokio::join!(a, b);
        ra.unwrap();
        rb.unwrap();
        let data = scheduled_read(&sched, IoClass::Foreground, &path, 0, 8)
            .await
            .unwrap();
        assert_eq!(&data[..], b"aaaabbbb");
    }

    #[tokio::test]
    async fn sparse_prepare_creates_parent_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("file.bin");
        let sched = hdd_scheduler();
        sched
            .prepare_file(&path, 12, PreallocationMode::Sparse)
            .await
            .unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 12);
    }

    #[tokio::test]
    async fn stats_track_io_sync_and_hash_work() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.bin");
        let sched = hdd_scheduler();

        scheduled_write(
            &sched,
            IoClass::PeerWrite,
            &path,
            0,
            bytes::Bytes::from_static(b"abcd"),
            true,
        )
        .await
        .unwrap();
        let data = scheduled_read(&sched, IoClass::PeerRead, &path, 0, 4)
            .await
            .unwrap();
        let hash = sched.hash_sha1(data).await.unwrap();
        assert_ne!(hash, [0; 20]);
        sched.sync_all_open_files().await.unwrap();

        let stats = sched.stats();
        assert_eq!(stats.write_ops_by_class[class_index(IoClass::PeerWrite)], 1);
        assert_eq!(stats.read_ops_by_class[class_index(IoClass::PeerRead)], 1);
        assert_eq!(
            stats.bytes_written_by_class[class_index(IoClass::PeerWrite)],
            4
        );
        assert_eq!(stats.bytes_read_by_class[class_index(IoClass::PeerRead)], 4);
        assert_eq!(stats.hash_ops, 1);
        assert_eq!(stats.sync_ops, 1);
        assert_eq!(stats.dirty_files, 0);
    }

    #[tokio::test]
    async fn peer_read_readahead_cache_returns_exact_requested_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("readahead.bin");
        std::fs::write(&path, b"abcdefghijklmnopqrstuvwxyz").unwrap();
        let sched = MountScheduler::new(
            StorageRootId::new(),
            &SchedulerConfig {
                profile: StorageProfile::Hdd,
                storage_io: StorageIoConfig {
                    peer_read_readahead_bytes: 16,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let first = scheduled_read(&sched, IoClass::PeerRead, &path, 0, 4)
            .await
            .unwrap();
        assert_eq!(&first[..], b"abcd");
        let second = scheduled_read(&sched, IoClass::PeerRead, &path, 4, 4)
            .await
            .unwrap();
        assert_eq!(&second[..], b"efgh");

        let stats = sched.stats();
        assert_eq!(stats.peer_read_cache_misses, 1);
        assert_eq!(stats.peer_read_cache_hits, 1);
        assert_eq!(stats.peer_read_cache_entries, 1);
        assert_eq!(
            stats.bytes_read_by_class[class_index(IoClass::PeerRead)],
            20
        );
    }
}
