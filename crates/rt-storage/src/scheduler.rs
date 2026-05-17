use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use tokio::sync::{
    mpsc::{self as tokio_mpsc, error::TrySendError},
    oneshot, OwnedSemaphorePermit, Semaphore, TryAcquireError,
};
use tracing::instrument;

use rt_hash::{merkle_root, BlockHash};
use rt_metrics::{MemoryClass, MemoryLease, ResourceGovernor};
use rt_path::{StorageProfile, StorageRootId};
use sha1::{Digest, Sha1};

use crate::{
    backend::{BackendRequest, DiskBackend, SelectedDiskBackend},
    device::{detect_storage_topology, StorageTopology},
    error::StorageError,
    frame::{global_frame_pool, Frame},
    io_class::IoClass,
};

pub const STORAGE_LATENCY_BUCKETS_NS: [u64; 8] = [
    100_000,
    500_000,
    1_000_000,
    5_000_000,
    10_000_000,
    50_000_000,
    100_000_000,
    u64::MAX,
];
pub const STORAGE_LATENCY_BUCKET_COUNT: usize = STORAGE_LATENCY_BUCKETS_NS.len();

const QUEUED_DISK_JOB_OVERHEAD_BYTES: u64 = 1024;

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
    Auto,
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
    pub peer_read_cache_entries: usize,
    pub peer_read_elevator_budget_ms: u64,
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
            preallocation_mode: PreallocationMode::Auto,
            durability_mode: DurabilityMode::Checkpoint,
            peer_read_readahead_bytes: 512 * 1024,
            peer_read_cache_entries: 64,
            peer_read_elevator_budget_ms: 25,
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
    pub resources: Option<ResourceGovernor>,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        SchedulerConfig {
            profile: StorageProfile::Unknown,
            max_queue: 256,
            recheck_concurrency: 0,
            peer_read_concurrency: 0,
            storage_io: StorageIoConfig::default(),
            resources: None,
        }
    }
}

fn effective_io_config_for_topology(
    io_config: &StorageIoConfig,
    topology: Option<&StorageTopology>,
) -> StorageIoConfig {
    let mut effective = io_config.clone();
    if effective.preallocation_mode == PreallocationMode::Auto {
        effective.preallocation_mode = preallocation_mode_for_topology(topology);
    }
    effective
}

pub fn preallocation_mode_for_topology(topology: Option<&StorageTopology>) -> PreallocationMode {
    match topology {
        Some(StorageTopology {
            profile: StorageProfile::Hdd,
            cow: false,
            ..
        }) => PreallocationMode::Full,
        _ => PreallocationMode::Sparse,
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FilePoolStats {
    pub capacity: usize,
    pub open_files: usize,
    pub memory_bytes: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub idle_closes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageIoStats {
    pub device_id: Option<String>,
    pub profile: StorageProfile,
    pub file_pool: FilePoolStats,
    pub io_queue_depth: usize,
    pub hash_queue_depth: usize,
    pub device_queue_capacity: usize,
    pub device_queue_available: usize,
    pub queued_disk_bytes: u64,
    pub queue_full: u64,
    pub dirty_files: usize,
    pub read_ops_by_class: [u64; 6],
    pub write_ops_by_class: [u64; 6],
    pub bytes_read_by_class: [u64; 6],
    pub bytes_written_by_class: [u64; 6],
    pub backend_read_ops_by_class: [u64; 6],
    pub backend_bytes_read_by_class: [u64; 6],
    pub read_latency_ns_by_class: [u64; 6],
    pub write_latency_ns_by_class: [u64; 6],
    pub read_latency_buckets: [u64; STORAGE_LATENCY_BUCKET_COUNT],
    pub write_latency_buckets: [u64; STORAGE_LATENCY_BUCKET_COUNT],
    pub sync_latency_buckets: [u64; STORAGE_LATENCY_BUCKET_COUNT],
    pub hash_latency_buckets: [u64; STORAGE_LATENCY_BUCKET_COUNT],
    pub sync_latency_ns: u64,
    pub hash_latency_ns: u64,
    pub sync_ops: u64,
    pub hash_ops: u64,
    pub preallocation_failures: u64,
    pub preallocation_fallbacks: u64,
    pub peer_read_cache_entries: usize,
    pub peer_read_cache_hits: u64,
    pub peer_read_cache_misses: u64,
    pub peer_read_cache_evictions: u64,
    pub peer_read_elevator_enabled: bool,
    pub peer_read_elevator_queue_depth: usize,
    pub peer_read_elevator_queued: usize,
    pub peer_read_elevator_queue_full: u64,
    pub peer_read_elevator_batches: u64,
    pub peer_read_elevator_coalesced_requests: u64,
    pub page_cache_advise_sequential: u64,
    pub page_cache_advise_willneed: u64,
    pub page_cache_advise_dontneed: u64,
    pub page_cache_advise_failures: u64,
    pub sparse_data_extents: u64,
    pub sparse_hole_bytes: u64,
    pub sparse_seek_fallbacks: u64,
}

/// Read payload that can keep ownership of a pooled storage frame.
///
/// `read_at` and `scheduled_read` remain `Bytes` compatibility wrappers.
/// New call sites that can process borrowed data should use
/// `read_owned_at` / `scheduled_read_owned` so exact backend reads can avoid
/// copying out of the process-wide frame pool.
#[derive(Debug)]
pub enum StorageRead {
    Frame(Frame),
    Bytes(bytes::Bytes),
}

impl StorageRead {
    pub fn len(&self) -> usize {
        match self {
            StorageRead::Frame(frame) => frame.len(),
            StorageRead::Bytes(bytes) => bytes.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        match self {
            StorageRead::Frame(frame) => frame.as_slice(),
            StorageRead::Bytes(bytes) => bytes.as_ref(),
        }
    }

    pub fn into_bytes(self) -> bytes::Bytes {
        match self {
            StorageRead::Frame(frame) => bytes::Bytes::copy_from_slice(frame.as_slice()),
            StorageRead::Bytes(bytes) => bytes,
        }
    }
}

impl Default for StorageIoStats {
    fn default() -> Self {
        Self {
            device_id: None,
            profile: StorageProfile::Unknown,
            file_pool: FilePoolStats::default(),
            io_queue_depth: 0,
            hash_queue_depth: 0,
            device_queue_capacity: 0,
            device_queue_available: 0,
            queued_disk_bytes: 0,
            queue_full: 0,
            dirty_files: 0,
            read_ops_by_class: [0; 6],
            write_ops_by_class: [0; 6],
            bytes_read_by_class: [0; 6],
            bytes_written_by_class: [0; 6],
            backend_read_ops_by_class: [0; 6],
            backend_bytes_read_by_class: [0; 6],
            read_latency_ns_by_class: [0; 6],
            write_latency_ns_by_class: [0; 6],
            read_latency_buckets: [0; STORAGE_LATENCY_BUCKET_COUNT],
            write_latency_buckets: [0; STORAGE_LATENCY_BUCKET_COUNT],
            sync_latency_buckets: [0; STORAGE_LATENCY_BUCKET_COUNT],
            hash_latency_buckets: [0; STORAGE_LATENCY_BUCKET_COUNT],
            sync_latency_ns: 0,
            hash_latency_ns: 0,
            sync_ops: 0,
            hash_ops: 0,
            preallocation_failures: 0,
            preallocation_fallbacks: 0,
            peer_read_cache_entries: 0,
            peer_read_cache_hits: 0,
            peer_read_cache_misses: 0,
            peer_read_cache_evictions: 0,
            peer_read_elevator_enabled: false,
            peer_read_elevator_queue_depth: 0,
            peer_read_elevator_queued: 0,
            peer_read_elevator_queue_full: 0,
            peer_read_elevator_batches: 0,
            peer_read_elevator_coalesced_requests: 0,
            page_cache_advise_sequential: 0,
            page_cache_advise_willneed: 0,
            page_cache_advise_dontneed: 0,
            page_cache_advise_failures: 0,
            sparse_data_extents: 0,
            sparse_hole_bytes: 0,
            sparse_seek_fallbacks: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataExtent {
    pub offset: u64,
    pub len: u64,
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

    fn open_for_sync(&self, path: &Path) -> Result<Arc<File>, StorageError> {
        let key = normalized_key(path);
        let path_str = key.display().to_string();
        let file = {
            let entries = self.entries.lock().expect("file pool mutex poisoned");
            entries
                .get(&key)
                .filter(|entry| entry.mode == OpenMode::Write)
                .map(|entry| entry.file.clone())
        };
        match file {
            Some(file) => Ok(file),
            None => Ok(Arc::new(
                OpenOptions::new()
                    .read(false)
                    .write(true)
                    .create(false)
                    .truncate(false)
                    .open(&key)
                    .map_err(|e| StorageError::io(&path_str, e))?,
            )),
        }
    }

    fn write_handles(&self) -> Vec<(PathBuf, Arc<File>)> {
        let entries = self.entries.lock().expect("file pool mutex poisoned");
        entries
            .iter()
            .filter(|(_, entry)| entry.mode == OpenMode::Write)
            .map(|(path, entry)| (path.clone(), entry.file.clone()))
            .collect()
    }

    fn stats(&self) -> FilePoolStats {
        let entries = self.entries.lock().expect("file pool mutex poisoned");
        FilePoolStats {
            capacity: self.capacity,
            open_files: entries.len(),
            memory_bytes: entries
                .keys()
                .map(|path| {
                    (std::mem::size_of::<PathBuf>() + path.as_os_str().as_encoded_bytes().len())
                        .saturating_add(std::mem::size_of::<CachedFile>())
                        as u64
                })
                .sum(),
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
    queue_name: &'static str,
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
        Self {
            queue_name: name_prefix,
            sender,
            queued,
        }
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
            .try_send(Box::new(move || {
                let _ = tx.send(f());
            }))
            .is_err()
        {
            self.queued.fetch_sub(1, Ordering::Relaxed);
            return Err(StorageError::QueueFull {
                mount: self.queue_name.to_string(),
            });
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
    device_queue_sem: Arc<Semaphore>,
    device_queue_capacity: usize,
    io_config: StorageIoConfig,
    file_pool: Arc<FilePool>,
    io_pool: Arc<BlockingPool>,
    disk_backend: Arc<SelectedDiskBackend>,
    hash_pool: Arc<BlockingPool>,
    dirty_paths: Arc<Mutex<HashSet<PathBuf>>>,
    peer_read_cache: Arc<Mutex<HashMap<PathBuf, PeerReadCacheEntry>>>,
    peer_read_elevator: Arc<Mutex<Option<PeerReadElevator>>>,
    peer_read_elevator_enabled: bool,
    peer_read_elevator_queue_depth: usize,
    device_id: Option<String>,
    profile: StorageProfile,
    resources: Option<ResourceGovernor>,
    counters: Arc<StorageCounters>,
}

#[derive(Debug)]
struct StorageCounters {
    read_ops_by_class: [AtomicU64; 6],
    write_ops_by_class: [AtomicU64; 6],
    bytes_read_by_class: [AtomicU64; 6],
    bytes_written_by_class: [AtomicU64; 6],
    backend_read_ops_by_class: [AtomicU64; 6],
    backend_bytes_read_by_class: [AtomicU64; 6],
    read_latency_ns_by_class: [AtomicU64; 6],
    write_latency_ns_by_class: [AtomicU64; 6],
    read_latency_buckets: [AtomicU64; STORAGE_LATENCY_BUCKET_COUNT],
    write_latency_buckets: [AtomicU64; STORAGE_LATENCY_BUCKET_COUNT],
    sync_latency_buckets: [AtomicU64; STORAGE_LATENCY_BUCKET_COUNT],
    hash_latency_buckets: [AtomicU64; STORAGE_LATENCY_BUCKET_COUNT],
    queued_disk_bytes: AtomicU64,
    queue_full: AtomicU64,
    sync_latency_ns: AtomicU64,
    hash_latency_ns: AtomicU64,
    sync_ops: AtomicU64,
    hash_ops: AtomicU64,
    preallocation_failures: AtomicU64,
    preallocation_fallbacks: AtomicU64,
    peer_read_cache_hits: AtomicU64,
    peer_read_cache_misses: AtomicU64,
    peer_read_cache_evictions: AtomicU64,
    peer_read_elevator_queue_full: AtomicU64,
    peer_read_elevator_batches: AtomicU64,
    peer_read_elevator_coalesced_requests: AtomicU64,
    page_cache_advise_sequential: AtomicU64,
    page_cache_advise_willneed: AtomicU64,
    page_cache_advise_dontneed: AtomicU64,
    page_cache_advise_failures: AtomicU64,
    sparse_data_extents: AtomicU64,
    sparse_hole_bytes: AtomicU64,
    sparse_seek_fallbacks: AtomicU64,
}

impl Default for StorageCounters {
    fn default() -> Self {
        Self {
            read_ops_by_class: std::array::from_fn(|_| AtomicU64::new(0)),
            write_ops_by_class: std::array::from_fn(|_| AtomicU64::new(0)),
            bytes_read_by_class: std::array::from_fn(|_| AtomicU64::new(0)),
            bytes_written_by_class: std::array::from_fn(|_| AtomicU64::new(0)),
            backend_read_ops_by_class: std::array::from_fn(|_| AtomicU64::new(0)),
            backend_bytes_read_by_class: std::array::from_fn(|_| AtomicU64::new(0)),
            read_latency_ns_by_class: std::array::from_fn(|_| AtomicU64::new(0)),
            write_latency_ns_by_class: std::array::from_fn(|_| AtomicU64::new(0)),
            read_latency_buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            write_latency_buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            sync_latency_buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            hash_latency_buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            queued_disk_bytes: AtomicU64::new(0),
            queue_full: AtomicU64::new(0),
            sync_latency_ns: AtomicU64::new(0),
            hash_latency_ns: AtomicU64::new(0),
            sync_ops: AtomicU64::new(0),
            hash_ops: AtomicU64::new(0),
            preallocation_failures: AtomicU64::new(0),
            preallocation_fallbacks: AtomicU64::new(0),
            peer_read_cache_hits: AtomicU64::new(0),
            peer_read_cache_misses: AtomicU64::new(0),
            peer_read_cache_evictions: AtomicU64::new(0),
            peer_read_elevator_queue_full: AtomicU64::new(0),
            peer_read_elevator_batches: AtomicU64::new(0),
            peer_read_elevator_coalesced_requests: AtomicU64::new(0),
            page_cache_advise_sequential: AtomicU64::new(0),
            page_cache_advise_willneed: AtomicU64::new(0),
            page_cache_advise_dontneed: AtomicU64::new(0),
            page_cache_advise_failures: AtomicU64::new(0),
            sparse_data_extents: AtomicU64::new(0),
            sparse_hole_bytes: AtomicU64::new(0),
            sparse_seek_fallbacks: AtomicU64::new(0),
        }
    }
}

#[derive(Debug)]
struct PeerReadCacheEntry {
    offset: u64,
    data: bytes::Bytes,
    last_used: Instant,
    _lease: Option<MemoryLease>,
}

#[derive(Debug, Clone)]
struct PeerReadElevator {
    sender: tokio_mpsc::Sender<PeerReadRequest>,
    counters: Arc<StorageCounters>,
    resources: Option<ResourceGovernor>,
}

#[derive(Debug)]
struct PeerReadRequest {
    path: PathBuf,
    offset: u64,
    len: usize,
    _queued_bytes: QueuedDiskBytes,
    tx: oneshot::Sender<Result<bytes::Bytes, StorageError>>,
}

#[derive(Debug)]
struct PeerReadBatch {
    path: PathBuf,
    offset: u64,
    len: usize,
    requests: Vec<PeerReadRequest>,
}

#[derive(Debug)]
struct QueuedDiskBytes {
    bytes: u64,
    counters: Arc<StorageCounters>,
    _lease: Option<MemoryLease>,
}

#[derive(Debug)]
struct DiskSubmission {
    _queued_bytes: QueuedDiskBytes,
    _queue: OwnedSemaphorePermit,
    _device_queue: OwnedSemaphorePermit,
}

#[derive(Debug)]
enum ReadPreparation {
    CacheHit(bytes::Bytes),
    BackendRead {
        key: PathBuf,
        file: Arc<File>,
        read_len: usize,
    },
}

impl QueuedDiskBytes {
    fn reserve(
        counters: Arc<StorageCounters>,
        resources: Option<&ResourceGovernor>,
        bytes: u64,
        mount: impl Into<String>,
    ) -> Result<Self, StorageError> {
        let bytes = queued_disk_charge(bytes);
        let lease = if let Some(resources) = resources {
            let Some(lease) = resources.try_acquire(MemoryClass::QueuedDisk, bytes) else {
                counters.queue_full.fetch_add(1, Ordering::Relaxed);
                return Err(StorageError::QueueFull {
                    mount: mount.into(),
                });
            };
            Some(lease)
        } else {
            None
        };
        if bytes > 0 {
            counters
                .queued_disk_bytes
                .fetch_add(bytes, Ordering::Relaxed);
        }
        Ok(Self {
            bytes,
            counters,
            _lease: lease,
        })
    }
}

impl Drop for QueuedDiskBytes {
    fn drop(&mut self) {
        if self.bytes > 0 {
            self.counters
                .queued_disk_bytes
                .fetch_sub(self.bytes, Ordering::Relaxed);
        }
    }
}

fn queued_disk_charge(payload_bytes: u64) -> u64 {
    payload_bytes.max(QUEUED_DISK_JOB_OVERHEAD_BYTES)
}

async fn await_backend_io<T>(
    path: &Path,
    rx: oneshot::Receiver<io::Result<T>>,
) -> Result<T, StorageError> {
    match rx.await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) if error.kind() == io::ErrorKind::WouldBlock => {
            Err(StorageError::QueueFull {
                mount: "storage-backend".to_string(),
            })
        }
        Ok(Err(error)) => Err(StorageError::io(path.display().to_string(), error)),
        Err(_) => Err(StorageError::Cancelled),
    }
}

static DEVICE_QUEUES: Lazy<Mutex<HashMap<String, Weak<Semaphore>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn device_queue_for(
    storage_root: StorageRootId,
    device_id: Option<&str>,
    profile: &StorageProfile,
    capacity: usize,
) -> Arc<Semaphore> {
    let key = device_queue_key(storage_root, device_id, profile);
    let mut queues = DEVICE_QUEUES
        .lock()
        .expect("device queue registry mutex poisoned");
    if let Some(queue) = queues.get(&key).and_then(Weak::upgrade) {
        return queue;
    }
    queues.retain(|_, queue| queue.strong_count() > 0);
    let queue = Arc::new(Semaphore::new(capacity.max(1)));
    queues.insert(key, Arc::downgrade(&queue));
    queue
}

fn device_queue_key(
    storage_root: StorageRootId,
    device_id: Option<&str>,
    profile: &StorageProfile,
) -> String {
    match device_id {
        Some(device_id) => format!("device:{profile:?}:{device_id}"),
        None => format!("storage-root:{}", storage_root.0),
    }
}

impl PeerReadElevator {
    fn spawn(
        storage_root: StorageRootId,
        queue_depth: usize,
        budget: Duration,
        file_pool: Arc<FilePool>,
        io_pool: Arc<BlockingPool>,
        disk_backend: Arc<SelectedDiskBackend>,
        queue_sem: Arc<Semaphore>,
        device_queue_sem: Arc<Semaphore>,
        counters: Arc<StorageCounters>,
        resources: Option<ResourceGovernor>,
    ) -> Self {
        let (sender, receiver) = tokio_mpsc::channel(queue_depth.max(1));
        tokio::spawn(peer_read_elevator_worker(
            storage_root,
            receiver,
            budget,
            file_pool,
            io_pool,
            disk_backend,
            queue_sem,
            device_queue_sem,
            counters.clone(),
        ));
        Self {
            sender,
            counters,
            resources,
        }
    }

    async fn read(
        &self,
        path: PathBuf,
        offset: u64,
        len: usize,
    ) -> Result<bytes::Bytes, StorageError> {
        self.try_enqueue(path, offset, len)?
            .await
            .map_err(|_| StorageError::Cancelled)?
    }

    fn try_enqueue(
        &self,
        path: PathBuf,
        offset: u64,
        len: usize,
    ) -> Result<oneshot::Receiver<Result<bytes::Bytes, StorageError>>, StorageError> {
        let (tx, rx) = oneshot::channel();
        let queued_bytes = QueuedDiskBytes::reserve(
            self.counters.clone(),
            self.resources.as_ref(),
            len as u64,
            "peer-read-elevator",
        )?;
        self.sender
            .try_send(PeerReadRequest {
                path,
                offset,
                len,
                _queued_bytes: queued_bytes,
                tx,
            })
            .map_err(|err| match err {
                TrySendError::Full(_) => {
                    self.counters
                        .peer_read_elevator_queue_full
                        .fetch_add(1, Ordering::Relaxed);
                    StorageError::QueueFull {
                        mount: "peer-read-elevator".to_string(),
                    }
                }
                TrySendError::Closed(_) => StorageError::Cancelled,
            })?;
        Ok(rx)
    }

    fn queued_len(&self) -> usize {
        self.sender
            .max_capacity()
            .saturating_sub(self.sender.capacity())
    }
}

async fn peer_read_elevator_worker(
    _storage_root: StorageRootId,
    mut receiver: tokio_mpsc::Receiver<PeerReadRequest>,
    budget: Duration,
    file_pool: Arc<FilePool>,
    io_pool: Arc<BlockingPool>,
    disk_backend: Arc<SelectedDiskBackend>,
    queue_sem: Arc<Semaphore>,
    device_queue_sem: Arc<Semaphore>,
    counters: Arc<StorageCounters>,
) {
    while let Some(first) = receiver.recv().await {
        let requests = collect_peer_read_batch(&mut receiver, first, budget).await;
        let batches = peer_read_batches(requests);
        for batch in batches {
            let file_pool = file_pool.clone();
            let io_pool = io_pool.clone();
            let disk_backend = disk_backend.clone();
            let queue_sem = queue_sem.clone();
            let device_queue_sem = device_queue_sem.clone();
            let counters = counters.clone();
            tokio::spawn(async move {
                let results = dispatch_peer_read_batch(
                    batch,
                    file_pool,
                    io_pool,
                    disk_backend,
                    queue_sem,
                    device_queue_sem,
                    counters,
                )
                .await;
                for (tx, result) in results {
                    let _ = tx.send(result);
                }
            });
        }
    }
}

async fn collect_peer_read_batch(
    receiver: &mut tokio_mpsc::Receiver<PeerReadRequest>,
    first: PeerReadRequest,
    budget: Duration,
) -> Vec<PeerReadRequest> {
    let started = Instant::now();
    let max_wait = budget.saturating_mul(4).max(budget);
    let quiet = Duration::from_millis(2);
    let mut requests = vec![first];

    loop {
        while let Ok(request) = receiver.try_recv() {
            requests.push(request);
        }

        let elapsed = started.elapsed();
        if budget.is_zero() || elapsed >= max_wait {
            break;
        }

        let wait = quiet.min(max_wait - elapsed);

        match tokio::time::timeout(wait, receiver.recv()).await {
            Ok(Some(request)) => requests.push(request),
            Ok(None) => break,
            Err(_) => break,
        }
    }

    requests
}

fn peer_read_batches(mut requests: Vec<PeerReadRequest>) -> Vec<PeerReadBatch> {
    requests.sort_by(|a, b| {
        normalized_key(&a.path)
            .cmp(&normalized_key(&b.path))
            .then_with(|| a.offset.cmp(&b.offset))
    });

    let mut batches: Vec<PeerReadBatch> = Vec::with_capacity(requests.len());
    for request in requests {
        let request_end = request.offset.saturating_add(request.len as u64);
        if let Some(last) = batches.last_mut() {
            let last_end = last.offset.saturating_add(last.len as u64);
            if normalized_key(&last.path) == normalized_key(&request.path)
                && request.offset <= last_end
            {
                last.len = request_end.saturating_sub(last.offset).max(last.len as u64) as usize;
                last.requests.push(request);
                continue;
            }
        }
        batches.push(PeerReadBatch {
            path: request.path.clone(),
            offset: request.offset,
            len: request.len,
            requests: vec![request],
        });
    }
    batches
}

async fn dispatch_peer_read_batch(
    batch: PeerReadBatch,
    file_pool: Arc<FilePool>,
    io_pool: Arc<BlockingPool>,
    disk_backend: Arc<SelectedDiskBackend>,
    queue_sem: Arc<Semaphore>,
    device_queue_sem: Arc<Semaphore>,
    counters: Arc<StorageCounters>,
) -> Vec<(
    oneshot::Sender<Result<bytes::Bytes, StorageError>>,
    Result<bytes::Bytes, StorageError>,
)> {
    let _queue = match queue_sem.acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => {
            return batch
                .requests
                .into_iter()
                .map(|request| (request.tx, Err(StorageError::Cancelled)))
                .collect();
        }
    };
    let _device_queue = match device_queue_sem.acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => {
            return batch
                .requests
                .into_iter()
                .map(|request| (request.tx, Err(StorageError::Cancelled)))
                .collect();
        }
    };

    let path = batch.path.clone();
    let offset = batch.offset;
    let len = batch.len;
    let read_counters = counters.clone();
    let read = match io_pool
        .run(move || {
            let key = normalized_key(&path);
            let file = file_pool.get_or_open(&key, OpenMode::Read, false)?;
            advise_for_read_class(&file, IoClass::PeerRead, offset, len, &read_counters);
            Ok::<_, StorageError>((key, file))
        })
        .await
    {
        Ok((key, file)) => {
            let frame = match global_frame_pool().try_acquire(len) {
                Some(frame) => frame,
                None => {
                    counters.queue_full.fetch_add(1, Ordering::Relaxed);
                    return batch
                        .requests
                        .into_iter()
                        .map(|request| {
                            (
                                request.tx,
                                Err(StorageError::QueueFull {
                                    mount: "scheduler-read-frame".to_string(),
                                }),
                            )
                        })
                        .collect();
                }
            };
            match await_backend_io(&key, disk_backend.pread(file.clone(), frame, offset)).await {
                Ok(frame) => {
                    advise_after_read_class(&file, IoClass::PeerRead, offset, len, &counters);
                    Ok(bytes::Bytes::copy_from_slice(frame.as_slice()))
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    };

    match read {
        Ok(bytes) => {
            counters
                .peer_read_elevator_batches
                .fetch_add(1, Ordering::Relaxed);
            counters.peer_read_elevator_coalesced_requests.fetch_add(
                batch.requests.len().saturating_sub(1) as u64,
                Ordering::Relaxed,
            );
            counters.backend_read_ops_by_class[class_index(IoClass::PeerRead)]
                .fetch_add(1, Ordering::Relaxed);
            counters.backend_bytes_read_by_class[class_index(IoClass::PeerRead)]
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
            batch
                .requests
                .into_iter()
                .map(|request| {
                    let relative = request.offset.saturating_sub(batch.offset) as usize;
                    let end = relative.saturating_add(request.len);
                    (request.tx, Ok(bytes.slice(relative..end)))
                })
                .collect()
        }
        Err(error) => {
            if matches!(error, StorageError::QueueFull { .. }) {
                counters.queue_full.fetch_add(1, Ordering::Relaxed);
            }
            let mut requests = batch.requests.into_iter();
            let Some(first) = requests.next() else {
                return Vec::new();
            };
            let mut out = vec![(first.tx, Err(error))];
            out.extend(requests.map(|request| (request.tx, Err(StorageError::Cancelled))));
            out
        }
    }
}

impl MountScheduler {
    pub fn new(storage_root: StorageRootId, config: &SchedulerConfig) -> Self {
        Self::new_with_profile_and_io_config(
            storage_root,
            config,
            config.profile.clone(),
            effective_io_config_for_topology(&config.storage_io, None),
            None,
        )
    }

    pub fn new_for_path(
        storage_root: StorageRootId,
        path: &Path,
        config: &SchedulerConfig,
    ) -> Self {
        let topology = detect_storage_topology(path);
        let profile = match &config.profile {
            StorageProfile::Unknown => topology.profile.clone(),
            profile => profile.clone(),
        };
        Self::new_with_profile_and_io_config(
            storage_root,
            config,
            profile,
            effective_io_config_for_topology(&config.storage_io, Some(&topology)),
            Some(topology),
        )
    }

    fn new_with_profile_and_io_config(
        storage_root: StorageRootId,
        config: &SchedulerConfig,
        profile: StorageProfile,
        io_config: StorageIoConfig,
        topology: Option<StorageTopology>,
    ) -> Self {
        let ssd = matches!(profile, StorageProfile::Ssd | StorageProfile::Nvme);
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
        let file_pool = Arc::new(FilePool::new(
            clamp_file_pool_size(io_config.file_pool_size),
            Duration::from_secs(io_config.idle_file_ttl_secs),
        ));
        let io_pool = Arc::new(BlockingPool::new(
            "rt-storage-io",
            io_config.io_worker_threads,
            io_config.io_queue_depth,
        ));
        let backend_request = std::env::var("TNG_STORAGE_BACKEND")
            .ok()
            .map(|value| BackendRequest::parse(&value))
            .unwrap_or(BackendRequest::Auto);
        let disk_backend = Arc::new(SelectedDiskBackend::select_with_queue_depth(
            backend_request,
            io_config.io_worker_threads,
            io_config.io_queue_depth,
        ));
        let queue_sem = Arc::new(Semaphore::new(
            config.max_queue.min(io_config.io_queue_depth).max(1),
        ));
        let device_id = topology
            .as_ref()
            .and_then(|topology| topology.device_id.as_ref().map(|device| device.0.clone()));
        let device_queue_capacity = config.max_queue.min(io_config.io_queue_depth).max(1);
        let device_queue_sem = device_queue_for(
            storage_root,
            device_id.as_deref(),
            &profile,
            device_queue_capacity,
        );
        let counters = Arc::new(StorageCounters::default());
        let peer_read_elevator_enabled =
            matches!(profile, StorageProfile::Hdd) && io_config.peer_read_elevator_budget_ms > 0;
        let peer_read_elevator_queue_depth = io_config.io_queue_depth.max(peer_read_limit);
        let peer_read_elevator =
            if peer_read_elevator_enabled && tokio::runtime::Handle::try_current().is_ok() {
                Some(PeerReadElevator::spawn(
                    storage_root,
                    peer_read_elevator_queue_depth,
                    Duration::from_millis(io_config.peer_read_elevator_budget_ms),
                    file_pool.clone(),
                    io_pool.clone(),
                    disk_backend.clone(),
                    queue_sem.clone(),
                    device_queue_sem.clone(),
                    counters.clone(),
                    config.resources.clone(),
                ))
            } else {
                None
            };
        let peer_read_elevator = Arc::new(Mutex::new(peer_read_elevator));

        MountScheduler {
            storage_root,
            recheck_sem: Arc::new(Semaphore::new(recheck_limit)),
            move_copy_sem: Arc::new(Semaphore::new(mv)),
            peer_write_sem: Arc::new(Semaphore::new(pw)),
            peer_read_sem: Arc::new(Semaphore::new(peer_read_limit)),
            foreground_sem: Arc::new(Semaphore::new(fg)),
            metadata_sem: Arc::new(Semaphore::new(md)),
            queue_sem,
            device_queue_sem,
            device_queue_capacity,
            file_pool,
            io_pool,
            disk_backend,
            hash_pool: Arc::new(BlockingPool::new(
                "rt-storage-hash",
                io_config.hash_worker_threads,
                io_config.hash_queue_depth,
            )),
            dirty_paths: Arc::new(Mutex::new(HashSet::new())),
            peer_read_cache: Arc::new(Mutex::new(HashMap::new())),
            peer_read_elevator,
            peer_read_elevator_enabled,
            peer_read_elevator_queue_depth,
            device_id,
            profile,
            resources: config.resources.clone(),
            counters,
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
        let peer_read_elevator_queued = self
            .peer_read_elevator
            .lock()
            .expect("peer read elevator mutex poisoned")
            .as_ref()
            .map(PeerReadElevator::queued_len)
            .unwrap_or(0);
        StorageIoStats {
            device_id: self.device_id.clone(),
            profile: self.profile.clone(),
            file_pool: self.file_pool.stats(),
            io_queue_depth: self.io_pool.queued(),
            hash_queue_depth: self.hash_pool.queued(),
            device_queue_capacity: self.device_queue_capacity,
            device_queue_available: self.device_queue_sem.available_permits(),
            queued_disk_bytes: self.counters.queued_disk_bytes.load(Ordering::Relaxed),
            queue_full: self.counters.queue_full.load(Ordering::Relaxed),
            dirty_files,
            read_ops_by_class: load_atomic_array(&self.counters.read_ops_by_class),
            write_ops_by_class: load_atomic_array(&self.counters.write_ops_by_class),
            bytes_read_by_class: load_atomic_array(&self.counters.bytes_read_by_class),
            bytes_written_by_class: load_atomic_array(&self.counters.bytes_written_by_class),
            backend_read_ops_by_class: load_atomic_array(&self.counters.backend_read_ops_by_class),
            backend_bytes_read_by_class: load_atomic_array(
                &self.counters.backend_bytes_read_by_class,
            ),
            read_latency_ns_by_class: load_atomic_array(&self.counters.read_latency_ns_by_class),
            write_latency_ns_by_class: load_atomic_array(&self.counters.write_latency_ns_by_class),
            read_latency_buckets: load_atomic_array(&self.counters.read_latency_buckets),
            write_latency_buckets: load_atomic_array(&self.counters.write_latency_buckets),
            sync_latency_buckets: load_atomic_array(&self.counters.sync_latency_buckets),
            hash_latency_buckets: load_atomic_array(&self.counters.hash_latency_buckets),
            sync_latency_ns: self.counters.sync_latency_ns.load(Ordering::Relaxed),
            hash_latency_ns: self.counters.hash_latency_ns.load(Ordering::Relaxed),
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
            peer_read_cache_evictions: self
                .counters
                .peer_read_cache_evictions
                .load(Ordering::Relaxed),
            peer_read_elevator_enabled: self.peer_read_elevator_enabled,
            peer_read_elevator_queue_depth: self.peer_read_elevator_queue_depth,
            peer_read_elevator_queued,
            peer_read_elevator_queue_full: self
                .counters
                .peer_read_elevator_queue_full
                .load(Ordering::Relaxed),
            peer_read_elevator_batches: self
                .counters
                .peer_read_elevator_batches
                .load(Ordering::Relaxed),
            peer_read_elevator_coalesced_requests: self
                .counters
                .peer_read_elevator_coalesced_requests
                .load(Ordering::Relaxed),
            page_cache_advise_sequential: self
                .counters
                .page_cache_advise_sequential
                .load(Ordering::Relaxed),
            page_cache_advise_willneed: self
                .counters
                .page_cache_advise_willneed
                .load(Ordering::Relaxed),
            page_cache_advise_dontneed: self
                .counters
                .page_cache_advise_dontneed
                .load(Ordering::Relaxed),
            page_cache_advise_failures: self
                .counters
                .page_cache_advise_failures
                .load(Ordering::Relaxed),
            sparse_data_extents: self.counters.sparse_data_extents.load(Ordering::Relaxed),
            sparse_hole_bytes: self.counters.sparse_hole_bytes.load(Ordering::Relaxed),
            sparse_seek_fallbacks: self.counters.sparse_seek_fallbacks.load(Ordering::Relaxed),
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

    async fn submit<T, F>(&self, queued_bytes: u64, f: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, StorageError> + Send + 'static,
    {
        let submission = self.reserve_submission(queued_bytes)?;
        let result = self
            .io_pool
            .run(move || {
                let _submission = submission;
                f()
            })
            .await;
        if matches!(result, Err(StorageError::QueueFull { .. })) {
            self.counters.queue_full.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    fn reserve_submission(&self, queued_bytes: u64) -> Result<DiskSubmission, StorageError> {
        let queued_bytes = QueuedDiskBytes::reserve(
            self.counters.clone(),
            self.resources.as_ref(),
            queued_bytes,
            format!("storage-root-{}", self.storage_root.0),
        )?;
        let queue = self
            .queue_sem
            .clone()
            .try_acquire_owned()
            .map_err(|err| match err {
                TryAcquireError::NoPermits => {
                    self.counters.queue_full.fetch_add(1, Ordering::Relaxed);
                    StorageError::QueueFull {
                        mount: format!("storage-root-{}", self.storage_root.0),
                    }
                }
                TryAcquireError::Closed => StorageError::Cancelled,
            })?;
        let device_queue = self
            .device_queue_sem
            .clone()
            .try_acquire_owned()
            .map_err(|err| match err {
                TryAcquireError::NoPermits => {
                    self.counters.queue_full.fetch_add(1, Ordering::Relaxed);
                    StorageError::QueueFull {
                        mount: self
                            .device_id
                            .as_ref()
                            .map(|device| format!("storage-device-{device}"))
                            .unwrap_or_else(|| format!("storage-root-{}", self.storage_root.0)),
                    }
                }
                TryAcquireError::Closed => StorageError::Cancelled,
            })?;
        Ok(DiskSubmission {
            _queued_bytes: queued_bytes,
            _queue: queue,
            _device_queue: device_queue,
        })
    }

    fn peer_read_elevator(&self) -> Option<PeerReadElevator> {
        if !self.peer_read_elevator_enabled {
            return None;
        }
        let mut elevator = self
            .peer_read_elevator
            .lock()
            .expect("peer read elevator mutex poisoned");
        if elevator.is_none() && tokio::runtime::Handle::try_current().is_ok() {
            *elevator = Some(PeerReadElevator::spawn(
                self.storage_root,
                self.peer_read_elevator_queue_depth,
                Duration::from_millis(self.io_config.peer_read_elevator_budget_ms),
                self.file_pool.clone(),
                self.io_pool.clone(),
                self.disk_backend.clone(),
                self.queue_sem.clone(),
                self.device_queue_sem.clone(),
                self.counters.clone(),
                self.resources.clone(),
            ));
        }
        elevator.clone()
    }

    pub async fn read_at(
        &self,
        class: IoClass,
        path: &Path,
        offset: u64,
        len: usize,
    ) -> Result<bytes::Bytes, StorageError> {
        self.read_owned_at(class, path, offset, len)
            .await
            .map(StorageRead::into_bytes)
    }

    pub async fn read_owned_at(
        &self,
        class: IoClass,
        path: &Path,
        offset: u64,
        len: usize,
    ) -> Result<StorageRead, StorageError> {
        let _permit = self.acquire(class).await?;
        let pool = self.file_pool.clone();
        let disk_backend = self.disk_backend.clone();
        let counters = self.counters.clone();
        let peer_read_cache = self.peer_read_cache.clone();
        let peer_read_elevator = self.peer_read_elevator();
        let readahead_bytes = self.io_config.peer_read_readahead_bytes;
        let readahead_cache_entries = self.io_config.peer_read_cache_entries;
        let path = path.to_path_buf();
        let started = Instant::now();
        if class == IoClass::PeerRead && readahead_bytes <= len {
            if let Some(elevator) = peer_read_elevator {
                let bytes = elevator.read(path, offset, len).await?;
                counters.read_ops_by_class[class_index(class)].fetch_add(1, Ordering::Relaxed);
                counters.bytes_read_by_class[class_index(class)]
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                let latency_ns = latency_ns_since(started);
                counters.read_latency_ns_by_class[class_index(class)]
                    .fetch_add(latency_ns, Ordering::Relaxed);
                record_latency_bucket(&counters.read_latency_buckets, latency_ns);
                return Ok(StorageRead::Bytes(bytes));
            }
        }
        let submission = self.reserve_submission(len as u64)?;
        let preparation_counters = counters.clone();
        let preparation_peer_read_cache = peer_read_cache.clone();
        let preparation = self
            .io_pool
            .run(move || {
                let key = normalized_key(&path);
                let path_str = key.display().to_string();
                if class == IoClass::PeerRead
                    && readahead_bytes > len
                    && readahead_cache_entries > 0
                {
                    if let Some(bytes) =
                        peer_read_cache_hit(&preparation_peer_read_cache, &key, offset, len)
                    {
                        preparation_counters
                            .peer_read_cache_hits
                            .fetch_add(1, Ordering::Relaxed);
                        preparation_counters.read_ops_by_class[class_index(class)]
                            .fetch_add(1, Ordering::Relaxed);
                        preparation_counters.bytes_read_by_class[class_index(class)]
                            .fetch_add(len as u64, Ordering::Relaxed);
                        let latency_ns = latency_ns_since(started);
                        preparation_counters.read_latency_ns_by_class[class_index(class)]
                            .fetch_add(latency_ns, Ordering::Relaxed);
                        record_latency_bucket(
                            &preparation_counters.read_latency_buckets,
                            latency_ns,
                        );
                        return Ok(ReadPreparation::CacheHit(bytes));
                    }
                    preparation_counters
                        .peer_read_cache_misses
                        .fetch_add(1, Ordering::Relaxed);
                }
                let file = pool.get_or_open(&key, OpenMode::Read, false)?;
                let read_len = if class == IoClass::PeerRead
                    && readahead_bytes > len
                    && readahead_cache_entries > 0
                {
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
                advise_for_read_class(&file, class, offset, read_len, &preparation_counters);
                Ok(ReadPreparation::BackendRead {
                    key,
                    file,
                    read_len,
                })
            })
            .await;
        if matches!(preparation, Err(StorageError::QueueFull { .. })) {
            self.counters.queue_full.fetch_add(1, Ordering::Relaxed);
        }
        let preparation = preparation?;
        match preparation {
            ReadPreparation::CacheHit(bytes) => Ok(StorageRead::Bytes(bytes)),
            ReadPreparation::BackendRead {
                key,
                file,
                read_len,
            } => {
                let frame = global_frame_pool().try_acquire(read_len).ok_or_else(|| {
                    StorageError::QueueFull {
                        mount: "scheduler-read-frame".to_string(),
                    }
                })?;
                let frame =
                    match await_backend_io(&key, disk_backend.pread(file.clone(), frame, offset))
                        .await
                    {
                        Ok(frame) => frame,
                        Err(error) => {
                            if matches!(error, StorageError::QueueFull { .. }) {
                                counters.queue_full.fetch_add(1, Ordering::Relaxed);
                            }
                            return Err(error);
                        }
                    };
                let _submission = submission;
                counters.read_ops_by_class[class_index(class)].fetch_add(1, Ordering::Relaxed);
                counters.bytes_read_by_class[class_index(class)]
                    .fetch_add(read_len as u64, Ordering::Relaxed);
                counters.backend_read_ops_by_class[class_index(class)]
                    .fetch_add(1, Ordering::Relaxed);
                counters.backend_bytes_read_by_class[class_index(class)]
                    .fetch_add(read_len as u64, Ordering::Relaxed);
                advise_after_read_class(&file, class, offset, read_len, &counters);
                let latency_ns = latency_ns_since(started);
                counters.read_latency_ns_by_class[class_index(class)]
                    .fetch_add(latency_ns, Ordering::Relaxed);
                record_latency_bucket(&counters.read_latency_buckets, latency_ns);
                if class == IoClass::PeerRead && read_len > len && readahead_cache_entries > 0 {
                    let bytes = bytes::Bytes::copy_from_slice(frame.as_slice());
                    let exact = bytes.slice(..len);
                    peer_read_cache_store(
                        &peer_read_cache,
                        &counters,
                        self.resources.as_ref(),
                        readahead_cache_entries,
                        key,
                        offset,
                        bytes,
                    );
                    Ok(StorageRead::Bytes(exact))
                } else {
                    Ok(StorageRead::Frame(frame))
                }
            }
        }
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
        let disk_backend = self.disk_backend.clone();
        let dirty_paths = self.dirty_paths.clone();
        let counters = self.counters.clone();
        let path = path.to_path_buf();
        let started = Instant::now();
        let submission = self.reserve_submission(data.len() as u64)?;
        let key = normalized_key(&path);
        let file = match self
            .io_pool
            .run({
                let pool = pool.clone();
                let key = key.clone();
                move || pool.get_or_open(&key, OpenMode::Write, create)
            })
            .await
        {
            Ok(file) => file,
            Err(error) => {
                if matches!(error, StorageError::QueueFull { .. }) {
                    counters.queue_full.fetch_add(1, Ordering::Relaxed);
                }
                return Err(error);
            }
        };
        let written = data.len();
        let result = await_backend_io(&key, disk_backend.pwrite(file.clone(), data, offset)).await;
        if let Err(error) = result {
            if matches!(error, StorageError::QueueFull { .. }) {
                counters.queue_full.fetch_add(1, Ordering::Relaxed);
            }
            return Err(error);
        }
        if strict {
            let result = await_backend_io(&key, disk_backend.fdatasync(file)).await;
            if let Err(error) = result {
                if matches!(error, StorageError::QueueFull { .. }) {
                    counters.queue_full.fetch_add(1, Ordering::Relaxed);
                }
                return Err(error);
            }
        } else {
            let mut dirty = dirty_paths.lock().expect("dirty path mutex poisoned");
            dirty.insert(key);
        }
        counters.write_ops_by_class[class_index(class)].fetch_add(1, Ordering::Relaxed);
        counters.bytes_written_by_class[class_index(class)]
            .fetch_add(written as u64, Ordering::Relaxed);
        let latency_ns = latency_ns_since(started);
        counters.write_latency_ns_by_class[class_index(class)]
            .fetch_add(latency_ns, Ordering::Relaxed);
        record_latency_bucket(&counters.write_latency_buckets, latency_ns);
        drop(submission);
        Ok(())
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
        self.submit(0, move || {
            let key = normalized_key(&path);
            let path_str = key.display().to_string();
            if let Some(parent) = key.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| StorageError::io(parent.display().to_string(), e))?;
            }
            let file = pool.get_or_open(&key, OpenMode::Write, true)?;
            match mode {
                PreallocationMode::Off => {}
                PreallocationMode::Auto => {
                    file.set_len(len)
                        .map_err(|e| StorageError::io(&path_str, e))?;
                }
                PreallocationMode::Sparse => {
                    file.set_len(len)
                        .map_err(|e| StorageError::io(&path_str, e))?;
                }
                PreallocationMode::Full => {
                    if let Err(e) = full_preallocate(&file, len) {
                        counters
                            .preallocation_fallbacks
                            .fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            component = "storage",
                            operation = "preallocate",
                            mode = "full",
                            result = "fallback",
                            error = %e,
                            "full preallocation failed; falling back to sparse length"
                        );
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
        let disk_backend = self.disk_backend.clone();
        let dirty_paths = self.dirty_paths.clone();
        let counters = self.counters.clone();
        let path = path.to_path_buf();
        let started = Instant::now();
        let submission = self.reserve_submission(0)?;
        let key = normalized_key(&path);
        let file = match self
            .io_pool
            .run({
                let pool = pool.clone();
                let key = key.clone();
                move || pool.open_for_sync(&key)
            })
            .await
        {
            Ok(file) => file,
            Err(error) => {
                if matches!(error, StorageError::QueueFull { .. }) {
                    counters.queue_full.fetch_add(1, Ordering::Relaxed);
                }
                return Err(error);
            }
        };
        let result = await_backend_io(&key, disk_backend.fdatasync(file)).await;
        if let Err(error) = result {
            if matches!(error, StorageError::QueueFull { .. }) {
                counters.queue_full.fetch_add(1, Ordering::Relaxed);
            }
            return Err(error);
        }
        counters.sync_ops.fetch_add(1, Ordering::Relaxed);
        let latency_ns = latency_ns_since(started);
        counters
            .sync_latency_ns
            .fetch_add(latency_ns, Ordering::Relaxed);
        record_latency_bucket(&counters.sync_latency_buckets, latency_ns);
        let mut dirty = dirty_paths.lock().expect("dirty path mutex poisoned");
        dirty.remove(&key);
        drop(submission);
        Ok(())
    }

    pub async fn sync_all_open_files(&self) -> Result<(), StorageError> {
        let pool = self.file_pool.clone();
        let disk_backend = self.disk_backend.clone();
        let dirty_paths = self.dirty_paths.clone();
        let counters = self.counters.clone();
        let started = Instant::now();
        let submission = self.reserve_submission(0)?;
        let paths: Vec<PathBuf> = {
            let dirty = dirty_paths.lock().expect("dirty path mutex poisoned");
            dirty.iter().cloned().collect()
        };
        let files = match self
            .io_pool
            .run({
                let pool = pool.clone();
                let paths = paths.clone();
                move || {
                    let mut files = pool.write_handles();
                    for path in paths {
                        let key = normalized_key(&path);
                        let file = pool.open_for_sync(&key)?;
                        if !files.iter().any(|(existing, _)| existing == &key) {
                            files.push((key, file));
                        }
                    }
                    Ok(files)
                }
            })
            .await
        {
            Ok(files) => files,
            Err(error) => {
                if matches!(error, StorageError::QueueFull { .. }) {
                    counters.queue_full.fetch_add(1, Ordering::Relaxed);
                }
                return Err(error);
            }
        };
        for (path, file) in files {
            let result = await_backend_io(&path, disk_backend.fdatasync(file)).await;
            if let Err(error) = result {
                if matches!(error, StorageError::QueueFull { .. }) {
                    counters.queue_full.fetch_add(1, Ordering::Relaxed);
                }
                return Err(error);
            }
        }
        counters.sync_ops.fetch_add(1, Ordering::Relaxed);
        let mut dirty = dirty_paths.lock().expect("dirty path mutex poisoned");
        for path in paths {
            dirty.remove(&path);
        }
        let latency_ns = latency_ns_since(started);
        counters
            .sync_latency_ns
            .fetch_add(latency_ns, Ordering::Relaxed);
        record_latency_bucket(&counters.sync_latency_buckets, latency_ns);
        drop(submission);
        Ok(())
    }

    pub async fn data_extents(
        &self,
        path: &Path,
        offset: u64,
        len: u64,
    ) -> Result<Vec<DataExtent>, StorageError> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let pool = self.file_pool.clone();
        let counters = self.counters.clone();
        let path = path.to_path_buf();
        self.submit(0, move || {
            let key = normalized_key(&path);
            let path_str = key.display().to_string();
            let file = pool.get_or_open(&key, OpenMode::Read, false)?;
            let file_len = file
                .metadata()
                .map_err(|e| StorageError::io(&path_str, e))?
                .len();
            let requested_end = offset.saturating_add(len);
            if offset >= file_len || requested_end > file_len {
                return Err(StorageError::ShortIo {
                    path: path_str,
                    expected: len as usize,
                    actual: file_len.saturating_sub(offset) as usize,
                });
            }
            let (extents, used_fallback) = seek_data_extents(&file, offset, len)
                .map_err(|e| StorageError::io(&path_str, e))?;
            if used_fallback {
                counters
                    .sparse_seek_fallbacks
                    .fetch_add(1, Ordering::Relaxed);
            }
            let data_bytes = extents.iter().map(|extent| extent.len).sum::<u64>();
            let hole_bytes = len.saturating_sub(data_bytes);
            counters
                .sparse_data_extents
                .fetch_add(extents.len() as u64, Ordering::Relaxed);
            counters
                .sparse_hole_bytes
                .fetch_add(hole_bytes, Ordering::Relaxed);
            Ok(extents)
        })
        .await
    }

    pub async fn hash_sha1(&self, data: bytes::Bytes) -> Result<[u8; 20], StorageError> {
        let counters = self.counters.clone();
        let started = Instant::now();
        let queued_bytes = QueuedDiskBytes::reserve(
            counters.clone(),
            self.resources.as_ref(),
            data.len() as u64,
            "rt-storage-hash",
        )?;
        let result = self
            .hash_pool
            .run(move || {
                let _queued_bytes = queued_bytes;
                let mut hasher = Sha1::new();
                hasher.update(&data);
                counters.hash_ops.fetch_add(1, Ordering::Relaxed);
                let latency_ns = latency_ns_since(started);
                counters
                    .hash_latency_ns
                    .fetch_add(latency_ns, Ordering::Relaxed);
                record_latency_bucket(&counters.hash_latency_buckets, latency_ns);
                Ok(hasher.finalize().into())
            })
            .await;
        if matches!(result, Err(StorageError::QueueFull { .. })) {
            self.counters.queue_full.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    pub async fn hash_v2_leaf(&self, data: bytes::Bytes) -> Result<[u8; 32], StorageError> {
        let counters = self.counters.clone();
        let started = Instant::now();
        let queued_bytes = QueuedDiskBytes::reserve(
            counters.clone(),
            self.resources.as_ref(),
            data.len() as u64,
            "rt-storage-hash",
        )?;
        let result = self
            .hash_pool
            .run(move || {
                let _queued_bytes = queued_bytes;
                counters.hash_ops.fetch_add(1, Ordering::Relaxed);
                let latency_ns = latency_ns_since(started);
                counters
                    .hash_latency_ns
                    .fetch_add(latency_ns, Ordering::Relaxed);
                record_latency_bucket(&counters.hash_latency_buckets, latency_ns);
                Ok(BlockHash::of(&data).0)
            })
            .await;
        if matches!(result, Err(StorageError::QueueFull { .. })) {
            self.counters.queue_full.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    pub async fn hash_v2_root(&self, leaves: Vec<[u8; 32]>) -> Result<[u8; 32], StorageError> {
        let counters = self.counters.clone();
        let started = Instant::now();
        let queued_bytes = QueuedDiskBytes::reserve(
            counters.clone(),
            self.resources.as_ref(),
            (leaves.len() * 32) as u64,
            "rt-storage-hash",
        )?;
        let result = self
            .hash_pool
            .run(move || {
                let _queued_bytes = queued_bytes;
                counters.hash_ops.fetch_add(1, Ordering::Relaxed);
                let latency_ns = latency_ns_since(started);
                counters
                    .hash_latency_ns
                    .fetch_add(latency_ns, Ordering::Relaxed);
                record_latency_bucket(&counters.hash_latency_buckets, latency_ns);
                Ok(merkle_root(&leaves))
            })
            .await;
        if matches!(result, Err(StorageError::QueueFull { .. })) {
            self.counters.queue_full.fetch_add(1, Ordering::Relaxed);
        }
        result
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

#[instrument(skip(scheduler), fields(path = %path.display(), offset, len))]
pub async fn scheduled_read_owned(
    scheduler: &MountScheduler,
    class: IoClass,
    path: &Path,
    offset: u64,
    len: usize,
) -> Result<StorageRead, StorageError> {
    scheduler.read_owned_at(class, path, offset, len).await
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

fn load_atomic_array<const N: usize>(values: &[AtomicU64; N]) -> [u64; N] {
    std::array::from_fn(|index| values[index].load(Ordering::Relaxed))
}

fn latency_ns_since(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

fn record_latency_bucket(buckets: &[AtomicU64; STORAGE_LATENCY_BUCKET_COUNT], latency_ns: u64) {
    for (index, upper_bound) in STORAGE_LATENCY_BUCKETS_NS.iter().enumerate() {
        if latency_ns <= *upper_bound {
            buckets[index].fetch_add(1, Ordering::Relaxed);
        }
    }
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
    counters: &StorageCounters,
    resources: Option<&ResourceGovernor>,
    max_entries: usize,
    key: PathBuf,
    offset: u64,
    data: bytes::Bytes,
) {
    if max_entries == 0 {
        return;
    }
    let lease = if let Some(resources) = resources {
        match resources.try_acquire(MemoryClass::PeerBuffer, data.len() as u64) {
            Some(lease) => Some(lease),
            None => {
                counters.queue_full.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
    } else {
        None
    };
    let mut cache = cache.lock().expect("peer read cache mutex poisoned");
    if !cache.contains_key(&key) && cache.len() >= max_entries {
        if let Some(evict) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&evict);
            counters
                .peer_read_cache_evictions
                .fetch_add(1, Ordering::Relaxed);
        }
    }
    cache.insert(
        key,
        PeerReadCacheEntry {
            offset,
            data,
            last_used: Instant::now(),
            _lease: lease,
        },
    );
}

fn advise_for_read_class(
    file: &File,
    class: IoClass,
    offset: u64,
    len: usize,
    counters: &StorageCounters,
) {
    if len < 256 * 1024 {
        return;
    }
    match class {
        IoClass::PeerRead | IoClass::Recheck => {
            if advise_page_cache(file, offset, len, PageCacheAdvice::Sequential) {
                counters
                    .page_cache_advise_sequential
                    .fetch_add(1, Ordering::Relaxed);
            } else {
                counters
                    .page_cache_advise_failures
                    .fetch_add(1, Ordering::Relaxed);
            }
            if class == IoClass::PeerRead {
                if advise_page_cache(file, offset, len, PageCacheAdvice::WillNeed) {
                    counters
                        .page_cache_advise_willneed
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    counters
                        .page_cache_advise_failures
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        _ => {}
    }
}

fn advise_after_read_class(
    file: &File,
    class: IoClass,
    offset: u64,
    len: usize,
    counters: &StorageCounters,
) {
    if class != IoClass::Recheck || len < 256 * 1024 {
        return;
    }
    if advise_page_cache(file, offset, len, PageCacheAdvice::DontNeed) {
        counters
            .page_cache_advise_dontneed
            .fetch_add(1, Ordering::Relaxed);
    } else {
        counters
            .page_cache_advise_failures
            .fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy)]
enum PageCacheAdvice {
    Sequential,
    WillNeed,
    DontNeed,
}

fn advise_page_cache(file: &File, offset: u64, len: usize, advice: PageCacheAdvice) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;

        let fd = file.as_raw_fd();
        let offset = offset.min(i64::MAX as u64) as libc::off_t;
        let len = len.min(i64::MAX as usize) as libc::off_t;
        let advice = match advice {
            PageCacheAdvice::Sequential => libc::POSIX_FADV_SEQUENTIAL,
            PageCacheAdvice::WillNeed => libc::POSIX_FADV_WILLNEED,
            PageCacheAdvice::DontNeed => libc::POSIX_FADV_DONTNEED,
        };
        unsafe { libc::posix_fadvise(fd, offset, len, advice) == 0 }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (file, offset, len, advice);
        true
    }
}

fn seek_data_extents(
    file: &File,
    offset: u64,
    len: u64,
) -> std::io::Result<(Vec<DataExtent>, bool)> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;

        let end = offset.saturating_add(len);
        if len == 0 {
            return Ok((Vec::new(), false));
        }

        let fd = file.as_raw_fd();
        let mut cursor = offset;
        let mut extents = Vec::new();
        while cursor < end {
            let data = unsafe {
                libc::lseek(
                    fd,
                    cursor.min(i64::MAX as u64) as libc::off_t,
                    libc::SEEK_DATA,
                )
            };
            if data < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ENXIO) {
                    break;
                }
                if matches!(err.raw_os_error(), Some(libc::EINVAL)) {
                    return Ok((vec![DataExtent { offset, len }], true));
                }
                return Err(err);
            }
            let data = (data as u64).max(cursor);
            if data >= end {
                break;
            }
            let hole = unsafe {
                libc::lseek(
                    fd,
                    data.min(i64::MAX as u64) as libc::off_t,
                    libc::SEEK_HOLE,
                )
            };
            if hole < 0 {
                let err = std::io::Error::last_os_error();
                if matches!(err.raw_os_error(), Some(libc::EINVAL)) {
                    return Ok((vec![DataExtent { offset, len }], true));
                }
                return Err(err);
            }
            let extent_end = (hole as u64).min(end);
            if extent_end > data {
                extents.push(DataExtent {
                    offset: data,
                    len: extent_end - data,
                });
            }
            cursor = extent_end.max(data.saturating_add(1));
        }
        Ok((extents, false))
    }

    #[cfg(not(target_os = "linux"))]
    {
        if len == 0 {
            Ok((Vec::new(), true))
        } else {
            Ok((vec![DataExtent { offset, len }], true))
        }
    }
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
    use crate::elevator::DeviceId;
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

    #[tokio::test]
    async fn full_mount_queue_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queued.bin");
        std::fs::write(&path, b"queued").unwrap();
        let sched = MountScheduler::new(
            StorageRootId::new(),
            &SchedulerConfig {
                profile: StorageProfile::Hdd,
                max_queue: 1,
                storage_io: StorageIoConfig {
                    io_queue_depth: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let _held_queue = sched.queue_sem.clone().try_acquire_owned().unwrap();

        let result = sched.read_at(IoClass::Foreground, &path, 0, 1).await;

        assert!(matches!(
            result,
            Err(StorageError::QueueFull { mount }) if mount.starts_with("storage-root-")
        ));
        assert_eq!(sched.stats().queue_full, 1);
        assert_eq!(sched.stats().queued_disk_bytes, 0);
    }

    #[tokio::test]
    async fn blocking_pool_full_queue_fails_closed() {
        let (sender, _receiver) = mpsc::sync_channel::<BlockingJob>(1);
        sender.send(Box::new(|| {})).unwrap();
        let pool = BlockingPool {
            queue_name: "test-blocking-pool",
            sender,
            queued: Arc::new(AtomicUsize::new(1)),
        };

        let result = pool.run(|| Ok::<_, StorageError>(())).await;

        assert!(matches!(
            result,
            Err(StorageError::QueueFull { mount }) if mount == "test-blocking-pool"
        ));
        assert_eq!(pool.queued(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queued_disk_bytes_track_active_blocking_job_payload() {
        let sched = hdd_scheduler();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let task = tokio::spawn({
            let sched = sched.clone();
            async move {
                sched
                    .submit(4096, move || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok::<_, StorageError>(())
                    })
                    .await
            }
        });

        started_rx.recv().unwrap();
        assert_eq!(sched.stats().queued_disk_bytes, 4096);

        release_tx.send(()).unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(sched.stats().queued_disk_bytes, 0);
    }

    #[tokio::test]
    async fn queued_disk_governor_denies_before_enqueue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queued-cap.bin");
        std::fs::write(&path, vec![1u8; 4096]).unwrap();
        let mut caps = [1024 * 1024; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::QueuedDisk as usize] = 1024;
        let resources = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: 1024 * 1024,
            class_caps_bytes: caps,
            ..Default::default()
        });
        let sched = MountScheduler::new(
            StorageRootId::new(),
            &SchedulerConfig {
                profile: StorageProfile::Ssd,
                resources: Some(resources.clone()),
                ..Default::default()
            },
        );

        let result = sched.read_at(IoClass::Foreground, &path, 0, 2048).await;

        assert!(matches!(
            result,
            Err(StorageError::QueueFull { mount }) if mount.starts_with("storage-root-")
        ));
        assert_eq!(sched.stats().queued_disk_bytes, 0);
        let snapshot = resources.snapshot();
        assert_eq!(
            snapshot.classes[MemoryClass::QueuedDisk as usize].denied_allocations,
            1
        );
    }

    #[tokio::test]
    async fn owned_read_returns_pooled_frame_for_exact_backend_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owned-read.bin");
        std::fs::write(&path, b"owned-frame").unwrap();
        let sched = MountScheduler::new(StorageRootId::new(), &SchedulerConfig::default());

        let read = scheduled_read_owned(&sched, IoClass::Foreground, &path, 0, 11)
            .await
            .unwrap();

        assert!(matches!(read, StorageRead::Frame(_)));
        assert_eq!(read.as_slice(), b"owned-frame");
    }

    #[tokio::test]
    async fn compatibility_read_still_returns_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compat-read.bin");
        std::fs::write(&path, b"compat").unwrap();
        let sched = MountScheduler::new(StorageRootId::new(), &SchedulerConfig::default());

        let read = scheduled_read(&sched, IoClass::Foreground, &path, 0, 6)
            .await
            .unwrap();

        assert_eq!(read, bytes::Bytes::from_static(b"compat"));
    }

    #[tokio::test]
    async fn schedulers_on_same_device_share_global_queue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload.bin");
        std::fs::write(&path, b"payload").unwrap();
        let topology = StorageTopology {
            device_id: Some(DeviceId("shared-device".to_owned())),
            profile: StorageProfile::Hdd,
            fs_type: Some("ext4".to_owned()),
            cow: false,
        };
        let config = SchedulerConfig {
            profile: StorageProfile::Hdd,
            max_queue: 1,
            storage_io: StorageIoConfig {
                io_queue_depth: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let first = MountScheduler::new_with_profile_and_io_config(
            StorageRootId::new(),
            &config,
            StorageProfile::Hdd,
            config.storage_io.clone(),
            Some(topology.clone()),
        );
        let second = MountScheduler::new_with_profile_and_io_config(
            StorageRootId::new(),
            &config,
            StorageProfile::Hdd,
            config.storage_io.clone(),
            Some(topology),
        );
        let _held = first.device_queue_sem.clone().try_acquire_owned().unwrap();

        let err = second
            .read_at(IoClass::Foreground, &path, 0, b"payload".len())
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            StorageError::QueueFull { mount } if mount == "storage-device-shared-device"
        ));
        let stats = second.stats();
        assert_eq!(stats.queue_full, 1);
        assert_eq!(stats.device_queue_capacity, 1);
        assert_eq!(stats.device_queue_available, 0);
    }

    #[test]
    fn ssd_has_higher_concurrency() {
        let hdd = hdd_scheduler();
        let ssd = ssd_scheduler();
        assert!(
            ssd.available_permits(IoClass::PeerRead) > hdd.available_permits(IoClass::PeerRead)
        );
    }

    #[test]
    fn auto_preallocation_policy_uses_full_only_for_non_cow_hdd() {
        let hdd = StorageTopology {
            device_id: Some(DeviceId("sda".to_owned())),
            profile: StorageProfile::Hdd,
            fs_type: Some("xfs".to_owned()),
            cow: false,
        };
        let hdd_cow = StorageTopology {
            device_id: Some(DeviceId("sdb".to_owned())),
            profile: StorageProfile::Hdd,
            fs_type: Some("btrfs".to_owned()),
            cow: true,
        };
        let nvme = StorageTopology {
            device_id: Some(DeviceId("nvme0n1".to_owned())),
            profile: StorageProfile::Nvme,
            fs_type: Some("ext4".to_owned()),
            cow: false,
        };

        assert_eq!(
            preallocation_mode_for_topology(Some(&hdd)),
            PreallocationMode::Full
        );
        assert_eq!(
            preallocation_mode_for_topology(Some(&hdd_cow)),
            PreallocationMode::Sparse
        );
        assert_eq!(
            preallocation_mode_for_topology(Some(&nvme)),
            PreallocationMode::Sparse
        );
        assert_eq!(
            preallocation_mode_for_topology(None),
            PreallocationMode::Sparse
        );
    }

    #[test]
    fn scheduler_new_resolves_auto_to_sparse_without_path_topology() {
        let sched = MountScheduler::new(
            StorageRootId::new(),
            &SchedulerConfig {
                profile: StorageProfile::Hdd,
                storage_io: StorageIoConfig {
                    preallocation_mode: PreallocationMode::Auto,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        assert_eq!(
            sched.io_config().preallocation_mode,
            PreallocationMode::Sparse
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
    async fn large_peer_and_recheck_reads_emit_page_cache_advice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.bin");
        std::fs::write(&path, vec![7u8; 512 * 1024]).unwrap();
        let sched = hdd_scheduler();

        let peer = scheduled_read(&sched, IoClass::PeerRead, &path, 0, 300 * 1024)
            .await
            .unwrap();
        assert_eq!(peer.len(), 300 * 1024);
        let recheck = scheduled_read(&sched, IoClass::Recheck, &path, 0, 300 * 1024)
            .await
            .unwrap();
        assert_eq!(recheck.len(), 300 * 1024);

        let stats = sched.stats();
        assert!(stats.page_cache_advise_sequential >= 2);
        assert!(stats.page_cache_advise_willneed >= 1);
        assert!(stats.page_cache_advise_dontneed >= 1);
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
            stats.backend_read_ops_by_class[class_index(IoClass::PeerRead)],
            1
        );
        assert_eq!(
            stats.bytes_written_by_class[class_index(IoClass::PeerWrite)],
            4
        );
        assert_eq!(stats.bytes_read_by_class[class_index(IoClass::PeerRead)], 4);
        assert_eq!(
            stats.backend_bytes_read_by_class[class_index(IoClass::PeerRead)],
            4
        );
        assert!(stats.read_latency_ns_by_class[class_index(IoClass::PeerRead)] > 0);
        assert!(stats.write_latency_ns_by_class[class_index(IoClass::PeerWrite)] > 0);
        assert_eq!(
            stats.read_latency_buckets[STORAGE_LATENCY_BUCKET_COUNT - 1],
            1
        );
        assert_eq!(
            stats.write_latency_buckets[STORAGE_LATENCY_BUCKET_COUNT - 1],
            1
        );
        assert_eq!(stats.hash_ops, 1);
        assert!(stats.hash_latency_ns > 0);
        assert_eq!(
            stats.hash_latency_buckets[STORAGE_LATENCY_BUCKET_COUNT - 1],
            1
        );
        assert_eq!(stats.queued_disk_bytes, 0);
        assert_eq!(stats.sync_ops, 1);
        assert!(stats.sync_latency_ns > 0);
        assert_eq!(
            stats.sync_latency_buckets[STORAGE_LATENCY_BUCKET_COUNT - 1],
            1
        );
        assert_eq!(stats.dirty_files, 0);
    }

    #[tokio::test]
    async fn sync_all_open_files_syncs_dirty_paths_after_fd_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.bin");
        let second = dir.path().join("second.bin");
        let sched = MountScheduler::new(
            StorageRootId::new(),
            &SchedulerConfig {
                profile: StorageProfile::Hdd,
                storage_io: StorageIoConfig {
                    file_pool_size: 1,
                    durability_mode: DurabilityMode::Checkpoint,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        sched
            .write_at(
                IoClass::PeerWrite,
                &first,
                0,
                bytes::Bytes::from_static(b"dirty-a"),
                true,
            )
            .await
            .unwrap();
        sched
            .write_at(
                IoClass::PeerWrite,
                &second,
                0,
                bytes::Bytes::from_static(b"dirty-b"),
                true,
            )
            .await
            .unwrap();

        let before_sync = sched.stats();
        assert_eq!(before_sync.file_pool.capacity, 1);
        assert_eq!(before_sync.file_pool.open_files, 1);
        assert!(before_sync.file_pool.evictions >= 1);
        assert_eq!(before_sync.dirty_files, 2);

        sched.sync_all_open_files().await.unwrap();

        let after_sync = sched.stats();
        assert_eq!(after_sync.dirty_files, 0);
        assert_eq!(after_sync.sync_ops, 1);
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
        assert_eq!(
            stats.backend_read_ops_by_class[class_index(IoClass::PeerRead)],
            1
        );
        assert_eq!(
            stats.backend_bytes_read_by_class[class_index(IoClass::PeerRead)],
            16
        );
    }

    #[tokio::test]
    async fn peer_read_readahead_cache_is_config_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let first_path = dir.path().join("first.bin");
        let second_path = dir.path().join("second.bin");
        std::fs::write(&first_path, b"abcdefghijklmnopqrstuvwxyz").unwrap();
        std::fs::write(&second_path, b"ABCDEFGHIJKLMNOPQRSTUVWXYZ").unwrap();
        let sched = MountScheduler::new(
            StorageRootId::new(),
            &SchedulerConfig {
                profile: StorageProfile::Hdd,
                storage_io: StorageIoConfig {
                    peer_read_readahead_bytes: 16,
                    peer_read_cache_entries: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        scheduled_read(&sched, IoClass::PeerRead, &first_path, 0, 4)
            .await
            .unwrap();
        scheduled_read(&sched, IoClass::PeerRead, &second_path, 0, 4)
            .await
            .unwrap();
        let first_after_eviction = scheduled_read(&sched, IoClass::PeerRead, &first_path, 4, 4)
            .await
            .unwrap();

        assert_eq!(&first_after_eviction[..], b"efgh");
        let stats = sched.stats();
        assert_eq!(stats.peer_read_cache_entries, 1);
        assert_eq!(stats.peer_read_cache_evictions, 2);
        assert_eq!(stats.peer_read_cache_misses, 3);
        assert_eq!(stats.peer_read_cache_hits, 0);
        assert_eq!(
            stats.backend_read_ops_by_class[class_index(IoClass::PeerRead)],
            3
        );
    }

    #[tokio::test]
    async fn peer_read_readahead_cache_can_be_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disabled-readahead.bin");
        std::fs::write(&path, b"abcdefghijklmnopqrstuvwxyz").unwrap();
        let sched = MountScheduler::new(
            StorageRootId::new(),
            &SchedulerConfig {
                profile: StorageProfile::Hdd,
                storage_io: StorageIoConfig {
                    peer_read_readahead_bytes: 16,
                    peer_read_cache_entries: 0,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let first = scheduled_read(&sched, IoClass::PeerRead, &path, 0, 4)
            .await
            .unwrap();
        let second = scheduled_read(&sched, IoClass::PeerRead, &path, 4, 4)
            .await
            .unwrap();

        assert_eq!(&first[..], b"abcd");
        assert_eq!(&second[..], b"efgh");
        let stats = sched.stats();
        assert_eq!(stats.peer_read_cache_entries, 0);
        assert_eq!(stats.peer_read_cache_evictions, 0);
        assert_eq!(stats.peer_read_cache_hits, 0);
        assert_eq!(stats.peer_read_cache_misses, 0);
        assert_eq!(
            stats.backend_read_ops_by_class[class_index(IoClass::PeerRead)],
            2
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn hdd_peer_read_elevator_batches_shuffled_adjacent_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("elevator.bin");
        let block_len = 1024usize;
        let blocks = 64usize;
        let data: Vec<u8> = (0..block_len * blocks).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let sched = MountScheduler::new(
            StorageRootId::new(),
            &SchedulerConfig {
                profile: StorageProfile::Hdd,
                peer_read_concurrency: blocks,
                storage_io: StorageIoConfig {
                    peer_read_readahead_bytes: 0,
                    peer_read_elevator_budget_ms: 5,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let mut set = tokio::task::JoinSet::new();
        for i in (0..blocks).rev() {
            let sched = sched.clone();
            let path = path.clone();
            set.spawn(async move {
                let offset = (i * block_len) as u64;
                let bytes = sched
                    .read_at(IoClass::PeerRead, &path, offset, block_len)
                    .await
                    .unwrap();
                (i, bytes)
            });
        }

        while let Some(joined) = set.join_next().await {
            let (i, bytes) = joined.unwrap();
            let start = i * block_len;
            assert_eq!(&bytes[..], &data[start..start + block_len]);
        }

        let stats = sched.stats();
        assert_eq!(
            stats.read_ops_by_class[class_index(IoClass::PeerRead)],
            blocks as u64
        );
        assert!(
            stats.backend_read_ops_by_class[class_index(IoClass::PeerRead)] * 5 <= blocks as u64,
            "expected elevator to reduce backend reads; stats={stats:?}"
        );
        assert!(stats.peer_read_elevator_enabled);
        assert!(stats.peer_read_elevator_queue_depth >= blocks);
        assert_eq!(stats.peer_read_elevator_queued, 0);
        assert!(stats.peer_read_elevator_batches > 0);
        assert!(
            stats.peer_read_elevator_coalesced_requests > 0,
            "expected elevator to coalesce requests; stats={stats:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hdd_peer_read_elevator_dispatches_after_quiet_slice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quiet-elevator.bin");
        std::fs::write(&path, vec![42_u8; 4096]).unwrap();
        let sched = MountScheduler::new(
            StorageRootId::new(),
            &SchedulerConfig {
                profile: StorageProfile::Hdd,
                storage_io: StorageIoConfig {
                    peer_read_readahead_bytes: 0,
                    peer_read_elevator_budget_ms: 250,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let started = Instant::now();
        let bytes = sched
            .read_at(IoClass::PeerRead, &path, 0, 4096)
            .await
            .unwrap();

        assert_eq!(bytes.len(), 4096);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "single peer read should not wait the full elevator budget"
        );
        let stats = sched.stats();
        assert_eq!(stats.peer_read_elevator_batches, 1);
        assert_eq!(stats.peer_read_elevator_coalesced_requests, 0);
    }

    #[test]
    fn peer_read_elevator_full_queue_fails_closed() {
        let (sender, _receiver) = tokio_mpsc::channel(1);
        let counters = Arc::new(StorageCounters::default());
        let elevator = PeerReadElevator {
            sender,
            counters: counters.clone(),
            resources: None,
        };
        let path = PathBuf::from("queued.bin");
        let first = elevator.try_enqueue(path.clone(), 0, 1024);
        assert!(first.is_ok());
        let second = elevator.try_enqueue(path, 1024, 1024);
        assert!(matches!(
            second,
            Err(StorageError::QueueFull { mount }) if mount == "peer-read-elevator"
        ));
        assert_eq!(
            counters
                .peer_read_elevator_queue_full
                .load(Ordering::Relaxed),
            1
        );
    }
}
