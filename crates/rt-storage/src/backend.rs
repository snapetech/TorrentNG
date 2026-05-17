//! Disk backend abstraction.
//!
//! All positioned I/O goes through a [`DiskBackend`]. Storage NG ships the
//! [`PreadBackend`] (a dedicated, bounded blocking thread pool calling
//! `pread`/`pwrite` via positioned I/O) as the portable default and a Linux
//! [`UringBackend`] for positioned `io_uring` reads, writes, and data sync.
//! Backend selection is explicit so older kernels and restricted containers
//! fall back cleanly.
//!
//! The pool is deliberately *separate* from Tokio's generic blocking pool
//! so disk I/O can neither starve nor be starved by unrelated
//! `spawn_blocking` work.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use io_uring::{opcode, types, IoUring};
use tokio::sync::oneshot;

use crate::frame::Frame;

const URING_ENTRIES: u32 = 256;
const URING_BATCH_LIMIT: usize = 64;
const URING_FILE_SLOTS: u32 = URING_ENTRIES;
const DEFAULT_BACKEND_QUEUE_DEPTH: usize = 1024;
/// Keep fixed buffer registration below common 8 MiB `RLIMIT_MEMLOCK`
/// defaults. The backend can still batch more submissions than it has
/// fixed-buffer slots; overflow submissions use ordinary read/write buffers.
const URING_FIXED_BUFFER_SLOTS: usize = 4;
const URING_FIXED_BUFFER_LEN: usize = 256 * 1024;

/// Backend requested by configuration or environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendRequest {
    /// Probe for the best supported backend.
    Auto,
    /// Force the portable positioned-I/O worker pool.
    Pread,
    /// Request the Linux `io_uring` backend, falling back when unavailable.
    Uring,
}

impl BackendRequest {
    /// Parse a user-facing backend name. Unknown values use `Auto` so typos do
    /// not break startup; the selected backend reason records the fallback.
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "pread" | "threadpool" | "thread-pool" => Self::Pread,
            "uring" | "io_uring" | "io-uring" => Self::Uring,
            "auto" | "" => Self::Auto,
            _ => Self::Auto,
        }
    }
}

/// Concrete backend selected after probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Pread,
    Uring,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pread => "pread",
            Self::Uring => "uring",
        }
    }
}

/// Probe result exposed for diagnostics and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSelection {
    pub requested: BackendRequest,
    pub selected: BackendKind,
    pub reason: String,
}

/// How the selected backend uses registered fixed buffers, when available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixedBufferStrategy {
    /// No fixed buffers are registered or used.
    Disabled,
    /// The backend registers private worker buffers and copies between those
    /// buffers and application frames at completion/submission time.
    WorkerCopy,
    /// The backend can submit application frame-pool slots directly.
    FramePoolSlots,
}

impl FixedBufferStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::WorkerCopy => "worker_copy",
            Self::FramePoolSlots => "frame_pool_slots",
        }
    }

    pub fn uses_worker_copy(self) -> bool {
        matches!(self, Self::WorkerCopy)
    }

    pub fn uses_frame_pool_slots(self) -> bool {
        matches!(self, Self::FramePoolSlots)
    }
}

/// Probe-selected disk backend.
///
/// The enum keeps the scheduler/runtime selection decision narrow while the
/// Linux backend grows from positioned SQEs and registered file slots to fixed
/// buffers.
pub struct SelectedDiskBackend {
    selection: BackendSelection,
    inner: SelectedDiskBackendInner,
}

enum SelectedDiskBackendInner {
    Pread(PreadBackend),
    Uring(UringBackend),
}

impl fmt::Debug for SelectedDiskBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SelectedDiskBackend")
            .field("selection", &self.selection)
            .finish_non_exhaustive()
    }
}

impl SelectedDiskBackend {
    /// Select and construct a backend with an explicit worker-thread budget.
    pub fn select(requested: BackendRequest, threads: usize) -> Self {
        Self::select_with_queue_depth(requested, threads, DEFAULT_BACKEND_QUEUE_DEPTH)
    }

    /// Select and construct a backend with explicit worker and queue budgets.
    pub fn select_with_queue_depth(
        requested: BackendRequest,
        threads: usize,
        queue_depth: usize,
    ) -> Self {
        match requested {
            BackendRequest::Pread => Self::pread(
                requested,
                threads,
                queue_depth,
                "forced by storage backend configuration".to_string(),
            ),
            BackendRequest::Auto => Self::pread(
                requested,
                threads,
                queue_depth,
                "auto uses pread baseline; request io_uring explicitly after correctness benchmarks"
                    .to_string(),
            ),
            BackendRequest::Uring => match UringBackend::probe() {
                Ok(probe) if probe.usable => {
                    Self::uring(requested, threads, queue_depth, probe.reason)
                }
                Ok(probe) => Self::pread(requested, threads, queue_depth, probe.reason),
                Err(reason) => Self::pread(requested, threads, queue_depth, reason),
            },
        }
    }

    /// Select using the default worker sizing for the fallback path.
    pub fn select_default(requested: BackendRequest) -> Self {
        let threads = default_worker_threads();
        Self::select(requested, threads)
    }

    pub fn selection(&self) -> &BackendSelection {
        &self.selection
    }

    pub fn kind(&self) -> BackendKind {
        self.selection.selected
    }

    fn pread(
        requested: BackendRequest,
        threads: usize,
        queue_depth: usize,
        reason: String,
    ) -> Self {
        let selection = BackendSelection {
            requested,
            selected: BackendKind::Pread,
            reason,
        };
        Self {
            selection,
            inner: SelectedDiskBackendInner::Pread(PreadBackend::new_with_queue_depth(
                threads,
                queue_depth,
            )),
        }
    }

    fn uring(
        requested: BackendRequest,
        threads: usize,
        queue_depth: usize,
        reason: String,
    ) -> Self {
        let backend = match UringBackend::try_new_with_queue_depth(threads, queue_depth) {
            Ok(backend) => backend,
            Err(error) => {
                return Self::pread(
                    requested,
                    threads,
                    queue_depth,
                    format!("{reason}; io_uring worker startup failed: {error}"),
                );
            }
        };
        let selection = BackendSelection {
            requested,
            selected: BackendKind::Uring,
            reason,
        };
        Self {
            selection,
            inner: SelectedDiskBackendInner::Uring(backend),
        }
    }
}

impl DiskBackend for SelectedDiskBackend {
    fn pread(
        &self,
        file: Arc<File>,
        frame: Frame,
        offset: u64,
    ) -> oneshot::Receiver<io::Result<Frame>> {
        match &self.inner {
            SelectedDiskBackendInner::Pread(backend) => backend.pread(file, frame, offset),
            SelectedDiskBackendInner::Uring(backend) => backend.pread(file, frame, offset),
        }
    }

    fn pwrite(
        &self,
        file: Arc<File>,
        data: bytes::Bytes,
        offset: u64,
    ) -> oneshot::Receiver<io::Result<()>> {
        match &self.inner {
            SelectedDiskBackendInner::Pread(backend) => backend.pwrite(file, data, offset),
            SelectedDiskBackendInner::Uring(backend) => backend.pwrite(file, data, offset),
        }
    }

    fn fdatasync(&self, file: Arc<File>) -> oneshot::Receiver<io::Result<()>> {
        match &self.inner {
            SelectedDiskBackendInner::Pread(backend) => backend.fdatasync(file),
            SelectedDiskBackendInner::Uring(backend) => backend.fdatasync(file),
        }
    }

    fn supports_fixed_buffers(&self) -> bool {
        match &self.inner {
            SelectedDiskBackendInner::Pread(backend) => backend.supports_fixed_buffers(),
            SelectedDiskBackendInner::Uring(backend) => backend.supports_fixed_buffers(),
        }
    }

    fn supports_registered_files(&self) -> bool {
        match &self.inner {
            SelectedDiskBackendInner::Pread(backend) => backend.supports_registered_files(),
            SelectedDiskBackendInner::Uring(backend) => backend.supports_registered_files(),
        }
    }

    fn max_batch_len(&self) -> usize {
        match &self.inner {
            SelectedDiskBackendInner::Pread(backend) => backend.max_batch_len(),
            SelectedDiskBackendInner::Uring(backend) => backend.max_batch_len(),
        }
    }

    fn fixed_buffer_len(&self) -> usize {
        match &self.inner {
            SelectedDiskBackendInner::Pread(backend) => backend.fixed_buffer_len(),
            SelectedDiskBackendInner::Uring(backend) => backend.fixed_buffer_len(),
        }
    }

    fn fixed_buffer_strategy(&self) -> FixedBufferStrategy {
        match &self.inner {
            SelectedDiskBackendInner::Pread(backend) => backend.fixed_buffer_strategy(),
            SelectedDiskBackendInner::Uring(backend) => backend.fixed_buffer_strategy(),
        }
    }
}

/// Probe information for the `io_uring` backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UringProbe {
    pub usable: bool,
    pub registered_files: bool,
    pub fixed_buffers: bool,
    pub reason: String,
}

/// Linux `io_uring` backend.
pub struct UringBackend {
    tx: mpsc::SyncSender<Job>,
    _workers: Vec<thread::JoinHandle<()>>,
    registered_files_supported: Arc<AtomicBool>,
    fixed_buffers_supported: Arc<AtomicBool>,
}

impl UringBackend {
    pub fn try_new(threads: usize) -> io::Result<Self> {
        Self::try_new_with_queue_depth(threads, DEFAULT_BACKEND_QUEUE_DEPTH)
    }

    pub fn try_new_with_queue_depth(threads: usize, queue_depth: usize) -> io::Result<Self> {
        let threads = threads.max(1);
        let (tx, rx) = mpsc::sync_channel::<Job>(queue_depth.max(1));
        let rx = Arc::new(Mutex::new(rx));
        let registered_files_supported = Arc::new(AtomicBool::new(false));
        let fixed_buffers_supported = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::with_capacity(threads);
        for i in 0..threads {
            let rx = Arc::clone(&rx);
            let registered_files_supported = Arc::clone(&registered_files_supported);
            let fixed_buffers_supported = Arc::clone(&fixed_buffers_supported);
            let (ready_tx, ready_rx) = mpsc::channel();
            let handle = thread::Builder::new()
                .name(format!("tng-uring-{i}"))
                .spawn(move || {
                    match UringWorker::new(rx, registered_files_supported, fixed_buffers_supported)
                    {
                        Ok(worker) => {
                            let _ = ready_tx.send(Ok(()));
                            worker.run();
                        }
                        Err(error) => {
                            let _ = ready_tx.send(Err(error));
                        }
                    }
                })
                .expect("spawn io_uring worker");
            match ready_rx.recv() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "io_uring worker exited during startup",
                    ));
                }
            }
            workers.push(handle);
        }
        Ok(Self {
            tx,
            _workers: workers,
            registered_files_supported,
            fixed_buffers_supported,
        })
    }

    pub fn probe() -> Result<UringProbe, String> {
        #[cfg(target_os = "linux")]
        {
            let disabled = std::fs::read_to_string("/proc/sys/kernel/io_uring_disabled")
                .ok()
                .and_then(|value| value.trim().parse::<u32>().ok())
                .unwrap_or(0);
            if disabled != 0 {
                return Ok(UringProbe {
                    usable: false,
                    registered_files: false,
                    fixed_buffers: false,
                    reason: format!("kernel reports io_uring disabled ({disabled})"),
                });
            }
            let ring = match IoUring::new(8) {
                Ok(ring) => ring,
                Err(e) => {
                    return Ok(UringProbe {
                        usable: false,
                        registered_files: false,
                        fixed_buffers: false,
                        reason: format!("io_uring probe failed: {e}"),
                    });
                }
            };
            let registered_files = probe_registered_files(&ring).is_ok();
            let fixed_buffers = probe_fixed_buffers(&ring).is_ok();
            let reason = format!(
                "io_uring probe succeeded; registered_files={registered_files} fixed_buffers={fixed_buffers}"
            );
            return Ok(UringProbe {
                usable: true,
                registered_files,
                fixed_buffers,
                reason,
            });
        }

        #[cfg(not(target_os = "linux"))]
        {
            Ok(UringProbe {
                usable: false,
                registered_files: false,
                fixed_buffers: false,
                reason: "io_uring is only available on Linux; using pread fallback".to_string(),
            })
        }
    }
}

fn probe_registered_files(ring: &IoUring) -> io::Result<()> {
    ring.submitter().register_files_sparse(1)?;
    ring.submitter().unregister_files()
}

fn probe_fixed_buffers(ring: &IoUring) -> io::Result<()> {
    let mut buffers = (0..URING_FIXED_BUFFER_SLOTS)
        .map(|_| vec![0u8; URING_FIXED_BUFFER_LEN])
        .collect::<Vec<_>>();
    let iovecs = buffers
        .iter_mut()
        .map(|buf| libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: buf.len(),
        })
        .collect::<Vec<_>>();
    unsafe {
        ring.submitter().register_buffers(&iovecs)?;
    }
    ring.submitter().unregister_buffers()
}

struct FixedBuffers {
    buffers: Vec<Vec<u8>>,
    free: Vec<u16>,
}

impl FixedBuffers {
    fn register(ring: &IoUring, slots: usize, len: usize) -> io::Result<Self> {
        let mut buffers = (0..slots).map(|_| vec![0u8; len]).collect::<Vec<_>>();
        let iovecs = buffers
            .iter_mut()
            .map(|buf| libc::iovec {
                iov_base: buf.as_mut_ptr().cast(),
                iov_len: buf.len(),
            })
            .collect::<Vec<_>>();
        unsafe {
            ring.submitter().register_buffers(&iovecs)?;
        }
        Ok(Self {
            buffers,
            free: (0..slots as u16).rev().collect(),
        })
    }

    fn acquire(&mut self, len: usize) -> Option<u16> {
        if len > URING_FIXED_BUFFER_LEN {
            return None;
        }
        self.free.pop()
    }

    fn release(&mut self, slot: u16) {
        self.free.push(slot);
    }

    fn ptr_mut(&mut self, slot: u16) -> *mut u8 {
        self.buffers[slot as usize].as_mut_ptr()
    }

    fn ptr(&self, slot: u16) -> *const u8 {
        self.buffers[slot as usize].as_ptr()
    }

    fn copy_from(&mut self, slot: u16, data: &[u8]) {
        self.buffers[slot as usize][..data.len()].copy_from_slice(data);
    }

    fn copy_to(&self, slot: u16, out: &mut [u8]) {
        out.copy_from_slice(&self.buffers[slot as usize][..out.len()]);
    }
}

impl DiskBackend for UringBackend {
    fn pread(
        &self,
        file: Arc<File>,
        frame: Frame,
        offset: u64,
    ) -> oneshot::Receiver<io::Result<Frame>> {
        let (reply, rx) = oneshot::channel();
        fail_job_on_full(self.tx.try_send(Job::Read {
            file,
            frame,
            offset,
            reply,
        }));
        rx
    }

    fn pwrite(
        &self,
        file: Arc<File>,
        data: bytes::Bytes,
        offset: u64,
    ) -> oneshot::Receiver<io::Result<()>> {
        let (reply, rx) = oneshot::channel();
        fail_job_on_full(self.tx.try_send(Job::Write {
            file,
            data,
            offset,
            reply,
        }));
        rx
    }

    fn fdatasync(&self, file: Arc<File>) -> oneshot::Receiver<io::Result<()>> {
        let (reply, rx) = oneshot::channel();
        fail_job_on_full(self.tx.try_send(Job::Sync { file, reply }));
        rx
    }

    fn supports_fixed_buffers(&self) -> bool {
        self.fixed_buffers_supported.load(Ordering::Relaxed)
    }

    fn supports_registered_files(&self) -> bool {
        self.registered_files_supported.load(Ordering::Relaxed)
    }

    fn max_batch_len(&self) -> usize {
        URING_BATCH_LIMIT
    }

    fn fixed_buffer_len(&self) -> usize {
        if self.supports_fixed_buffers() {
            URING_FIXED_BUFFER_LEN
        } else {
            0
        }
    }

    fn fixed_buffer_strategy(&self) -> FixedBufferStrategy {
        if self.supports_fixed_buffers() {
            FixedBufferStrategy::WorkerCopy
        } else {
            FixedBufferStrategy::Disabled
        }
    }
}

struct UringWorker {
    rx: Arc<Mutex<mpsc::Receiver<Job>>>,
    ring: IoUring,
    fixed: Option<FixedBuffers>,
    next_id: u64,
    registered_files: bool,
    file_slots: HashMap<FileIdentity, u32>,
    slot_files: Vec<Option<FileIdentity>>,
    pending: HashMap<u64, PendingUring>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
}

fn file_identity(file: &File) -> Option<FileIdentity> {
    let metadata = file.metadata().ok()?;
    Some(FileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

enum PendingUring {
    Read {
        file: Arc<File>,
        frame: Frame,
        expected: usize,
        fixed_slot: Option<u16>,
        reply: oneshot::Sender<io::Result<Frame>>,
    },
    Write {
        file: Arc<File>,
        data: bytes::Bytes,
        expected: usize,
        fixed_slot: Option<u16>,
        reply: oneshot::Sender<io::Result<()>>,
    },
    Sync {
        file: Arc<File>,
        reply: oneshot::Sender<io::Result<()>>,
    },
}

impl UringWorker {
    fn new(
        rx: Arc<Mutex<mpsc::Receiver<Job>>>,
        registered_files_supported: Arc<AtomicBool>,
        fixed_buffers_supported: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        let ring = IoUring::new(URING_ENTRIES)?;
        let registered_files = match ring.submitter().register_files_sparse(URING_FILE_SLOTS) {
            Ok(()) => {
                registered_files_supported.store(true, Ordering::Relaxed);
                true
            }
            Err(e) => {
                tracing::debug!(
                    component = "storage",
                    operation = "io_uring_register_files",
                    result = "unavailable",
                    error = %e,
                    "io_uring fixed-file table unavailable"
                );
                false
            }
        };
        let fixed =
            match FixedBuffers::register(&ring, URING_FIXED_BUFFER_SLOTS, URING_FIXED_BUFFER_LEN) {
                Ok(fixed) => {
                    fixed_buffers_supported.store(true, Ordering::Relaxed);
                    Some(fixed)
                }
                Err(e) => {
                    tracing::debug!(
                        component = "storage",
                        operation = "io_uring_register_buffers",
                        result = "unavailable",
                        error = %e,
                        "io_uring fixed-buffer registration unavailable"
                    );
                    None
                }
            };
        Ok(Self {
            rx,
            ring,
            fixed,
            next_id: 1,
            registered_files,
            file_slots: HashMap::new(),
            slot_files: vec![None; URING_FILE_SLOTS as usize],
            pending: HashMap::new(),
        })
    }

    fn run(mut self) {
        while let Some(first) = self.recv_job() {
            let jobs = self.recv_batch(first);
            let mut submitted = 0;
            for job in jobs {
                match self.submit_job(job) {
                    Ok(()) => submitted += 1,
                    Err(e) => tracing::warn!(
                        component = "storage",
                        operation = "io_uring_submit",
                        result = "error",
                        error = %e,
                        "io_uring submission failed"
                    ),
                }
            }
            if submitted == 0 {
                continue;
            }
            if let Err(e) = self.ring.submit_and_wait(submitted) {
                self.fail_all(e);
                continue;
            }
            self.complete_ready();
        }
    }

    fn recv_job(&self) -> Option<Job> {
        let guard = self.rx.lock().expect("uring job queue poisoned");
        guard.recv().ok()
    }

    fn recv_batch(&self, first: Job) -> Vec<Job> {
        let mut jobs = Vec::with_capacity(URING_BATCH_LIMIT);
        jobs.push(first);
        let guard = self.rx.lock().expect("uring job queue poisoned");
        while jobs.len() < URING_BATCH_LIMIT {
            match guard.try_recv() {
                Ok(job) => jobs.push(job),
                Err(mpsc::TryRecvError::Empty) | Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        jobs
    }

    fn submit_job(&mut self, job: Job) -> io::Result<()> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        match job {
            Job::Read {
                file,
                mut frame,
                offset,
                reply,
            } => {
                let len = frame.len();
                let file_slot = self.register_file_slot(&file);
                let fixed_slot = self.fixed.as_mut().and_then(|fixed| fixed.acquire(len));
                let entry = if let Some(buf_slot) = fixed_slot {
                    let ptr = self
                        .fixed
                        .as_mut()
                        .expect("fixed slot from set")
                        .ptr_mut(buf_slot);
                    match file_slot {
                        Some(file_slot) => {
                            opcode::ReadFixed::new(types::Fixed(file_slot), ptr, len as _, buf_slot)
                        }
                        None => opcode::ReadFixed::new(
                            types::Fd(file.as_raw_fd()),
                            ptr,
                            len as _,
                            buf_slot,
                        ),
                    }
                    .offset(offset)
                    .build()
                    .user_data(id)
                } else {
                    let ptr = frame.as_mut_slice().as_mut_ptr();
                    match file_slot {
                        Some(slot) => opcode::Read::new(types::Fixed(slot), ptr, len as _),
                        None => opcode::Read::new(types::Fd(file.as_raw_fd()), ptr, len as _),
                    }
                    .offset(offset)
                    .build()
                    .user_data(id)
                };
                if let Err(e) = self.push_entry(entry) {
                    self.release_submission_resources(file_slot, fixed_slot);
                    return Err(e);
                }
                self.pending.insert(
                    id,
                    PendingUring::Read {
                        file,
                        frame,
                        expected: len,
                        fixed_slot,
                        reply,
                    },
                );
            }
            Job::Write {
                file,
                data,
                offset,
                reply,
            } => {
                let len = data.len();
                let file_slot = self.register_file_slot(&file);
                let fixed_slot = self.fixed.as_mut().and_then(|fixed| fixed.acquire(len));
                let entry = if let Some(buf_slot) = fixed_slot {
                    let fixed = self.fixed.as_mut().expect("fixed slot from set");
                    fixed.copy_from(buf_slot, &data);
                    match file_slot {
                        Some(file_slot) => opcode::WriteFixed::new(
                            types::Fixed(file_slot),
                            fixed.ptr(buf_slot),
                            len as _,
                            buf_slot,
                        ),
                        None => opcode::WriteFixed::new(
                            types::Fd(file.as_raw_fd()),
                            fixed.ptr(buf_slot),
                            len as _,
                            buf_slot,
                        ),
                    }
                    .offset(offset)
                    .build()
                    .user_data(id)
                } else {
                    let ptr = data.as_ptr();
                    match file_slot {
                        Some(slot) => opcode::Write::new(types::Fixed(slot), ptr, len as _),
                        None => opcode::Write::new(types::Fd(file.as_raw_fd()), ptr, len as _),
                    }
                    .offset(offset)
                    .build()
                    .user_data(id)
                };
                if let Err(e) = self.push_entry(entry) {
                    self.release_submission_resources(file_slot, fixed_slot);
                    return Err(e);
                }
                self.pending.insert(
                    id,
                    PendingUring::Write {
                        file,
                        data,
                        expected: len,
                        fixed_slot,
                        reply,
                    },
                );
            }
            Job::Sync { file, reply } => {
                let file_slot = self.register_file_slot(&file);
                let entry = match file_slot {
                    Some(slot) => opcode::Fsync::new(types::Fixed(slot)),
                    None => opcode::Fsync::new(types::Fd(file.as_raw_fd())),
                }
                .flags(types::FsyncFlags::DATASYNC)
                .build()
                .user_data(id);
                if let Err(e) = self.push_entry(entry) {
                    return Err(e);
                }
                self.pending.insert(id, PendingUring::Sync { file, reply });
            }
        }
        Ok(())
    }

    fn register_file_slot(&mut self, file: &File) -> Option<u32> {
        if !self.registered_files {
            return None;
        }
        let identity = file_identity(file)?;
        if let Some(slot) = self.file_slots.get(&identity) {
            return Some(*slot);
        }
        let slot = self.slot_files.iter().position(Option::is_none)? as u32;
        match self
            .ring
            .submitter()
            .register_files_update(slot, &[file.as_raw_fd()])
        {
            Ok(1) => {
                self.slot_files[slot as usize] = Some(identity.clone());
                self.file_slots.insert(identity, slot);
                Some(slot)
            }
            Ok(_) => None,
            Err(e) => {
                tracing::debug!(
                    component = "storage",
                    operation = "io_uring_update_fixed_file",
                    slot,
                    result = "error",
                    error = %e,
                    "io_uring fixed-file update failed"
                );
                None
            }
        }
    }

    fn release_fixed_slot(&mut self, slot: Option<u16>) {
        if let Some(slot) = slot {
            self.fixed
                .as_mut()
                .expect("fixed slot came from registered buffer set")
                .release(slot);
        }
    }

    fn release_submission_resources(&mut self, _file_slot: Option<u32>, fixed_slot: Option<u16>) {
        self.release_fixed_slot(fixed_slot);
    }

    fn push_entry(&mut self, entry: io_uring::squeue::Entry) -> io::Result<()> {
        unsafe {
            self.ring
                .submission()
                .push(&entry)
                .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "io_uring SQ full"))
        }
    }

    fn complete_ready(&mut self) {
        let completions = self
            .ring
            .completion()
            .map(|cqe| (cqe.user_data(), cqe.result()));
        for (id, result) in completions.collect::<Vec<_>>() {
            let Some(pending) = self.pending.remove(&id) else {
                continue;
            };
            match pending {
                PendingUring::Read {
                    file,
                    mut frame,
                    expected,
                    fixed_slot,
                    reply,
                } => {
                    let _keepalive = file;
                    let mut fixed_returned = false;
                    let _ = reply.send(match uring_result(result) {
                        Ok(n) if n == expected => {
                            if let Some(slot) = fixed_slot {
                                let fixed = self.fixed.as_mut().expect("fixed slot from set");
                                fixed.copy_to(slot, frame.as_mut_slice());
                                fixed.release(slot);
                                fixed_returned = true;
                            }
                            Ok(frame)
                        }
                        Ok(_) => Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "short io_uring read",
                        )),
                        Err(e) => Err(e),
                    });
                    if !fixed_returned {
                        self.release_fixed_slot(fixed_slot);
                    }
                }
                PendingUring::Write {
                    file,
                    data,
                    expected,
                    fixed_slot,
                    reply,
                } => {
                    let _keepalive_file = file;
                    let _keepalive = data;
                    self.release_fixed_slot(fixed_slot);
                    let _ = reply.send(match uring_result(result) {
                        Ok(n) if n == expected => Ok(()),
                        Ok(_) => Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "short io_uring write",
                        )),
                        Err(e) => Err(e),
                    });
                }
                PendingUring::Sync { file, reply } => {
                    let _keepalive = file;
                    let _ = reply.send(uring_result(result).map(|_| ()));
                }
            }
        }
    }

    fn fail_all(&mut self, err: io::Error) {
        let message = err.to_string();
        let pending = self
            .pending
            .drain()
            .map(|(_, pending)| pending)
            .collect::<Vec<_>>();
        for pending in pending {
            let e = || io::Error::new(err.kind(), message.clone());
            match pending {
                PendingUring::Read {
                    reply, fixed_slot, ..
                } => {
                    self.release_fixed_slot(fixed_slot);
                    let _ = reply.send(Err(e()));
                }
                PendingUring::Write {
                    reply, fixed_slot, ..
                } => {
                    self.release_fixed_slot(fixed_slot);
                    let _ = reply.send(Err(e()));
                }
                PendingUring::Sync { reply, .. } => {
                    let _ = reply.send(Err(e()));
                }
            }
        }
    }
}

fn uring_result(result: i32) -> io::Result<usize> {
    if result < 0 {
        Err(io::Error::from_raw_os_error(-result))
    } else {
        Ok(result as usize)
    }
}

/// Positioned disk operations. Implementors must be safe to call
/// concurrently against the same file handle (true for `pread`/`pwrite`).
pub trait DiskBackend: Send + Sync {
    /// Read exactly `frame.len()` bytes at `offset` into `frame`.
    fn pread(
        &self,
        file: Arc<File>,
        frame: Frame,
        offset: u64,
    ) -> oneshot::Receiver<io::Result<Frame>>;

    /// Write all of `data` at `offset`.
    fn pwrite(
        &self,
        file: Arc<File>,
        data: bytes::Bytes,
        offset: u64,
    ) -> oneshot::Receiver<io::Result<()>>;

    /// Flush file data (not necessarily metadata) to the device.
    fn fdatasync(&self, file: Arc<File>) -> oneshot::Receiver<io::Result<()>>;

    /// Whether the backend can register fixed buffers (io_uring). The
    /// pread backend cannot; the elevator uses this to decide pinning.
    fn supports_fixed_buffers(&self) -> bool {
        false
    }

    /// Whether the backend can use registered file slots.
    fn supports_registered_files(&self) -> bool {
        false
    }

    /// Maximum number of jobs the backend submits as one batch.
    fn max_batch_len(&self) -> usize {
        1
    }

    /// Size of each registered fixed buffer, or 0 when unsupported.
    fn fixed_buffer_len(&self) -> usize {
        0
    }

    /// Whether fixed-buffer submissions use backend-private copy buffers or
    /// application frame-pool slots directly.
    fn fixed_buffer_strategy(&self) -> FixedBufferStrategy {
        FixedBufferStrategy::Disabled
    }
}

enum Job {
    Read {
        file: Arc<File>,
        frame: Frame,
        offset: u64,
        reply: oneshot::Sender<io::Result<Frame>>,
    },
    Write {
        file: Arc<File>,
        data: bytes::Bytes,
        offset: u64,
        reply: oneshot::Sender<io::Result<()>>,
    },
    Sync {
        file: Arc<File>,
        reply: oneshot::Sender<io::Result<()>>,
    },
}

/// `pread`/`pwrite` backend backed by a fixed pool of dedicated OS
/// threads. Threads block on real syscalls; the async caller awaits a
/// `oneshot`.
pub struct PreadBackend {
    tx: mpsc::SyncSender<Job>,
    _workers: Vec<thread::JoinHandle<()>>,
}

impl PreadBackend {
    /// Spawn `threads` dedicated I/O workers (clamped to at least 1).
    pub fn new(threads: usize) -> Self {
        Self::new_with_queue_depth(threads, DEFAULT_BACKEND_QUEUE_DEPTH)
    }

    pub fn new_with_queue_depth(threads: usize, queue_depth: usize) -> Self {
        let threads = threads.max(1);
        let (tx, rx) = mpsc::sync_channel::<Job>(queue_depth.max(1));
        let rx = Arc::new(Mutex::new(rx));
        let mut workers = Vec::with_capacity(threads);
        for i in 0..threads {
            let rx = Arc::clone(&rx);
            let handle = thread::Builder::new()
                .name(format!("tng-disk-{i}"))
                .spawn(move || Self::worker(rx))
                .expect("spawn disk worker");
            workers.push(handle);
        }
        PreadBackend {
            tx,
            _workers: workers,
        }
    }

    /// Default sizing: scale with cores but stay modest — disk, not CPU,
    /// is the constraint. The elevator above bounds true concurrency.
    pub fn with_default_threads() -> Self {
        Self::new(default_worker_threads())
    }

    fn worker(rx: Arc<Mutex<mpsc::Receiver<Job>>>) {
        loop {
            // Lock only to dequeue; the actual syscall runs unlocked so
            // workers process jobs in parallel.
            let job = {
                let guard = rx.lock().expect("disk job queue poisoned");
                guard.recv()
            };
            let Ok(job) = job else {
                return; // sender dropped → shutdown
            };
            match job {
                Job::Read {
                    file,
                    mut frame,
                    offset,
                    reply,
                } => {
                    let res = file
                        .read_exact_at(frame.as_mut_slice(), offset)
                        .map(|()| frame);
                    let _ = reply.send(res);
                }
                Job::Write {
                    file,
                    data,
                    offset,
                    reply,
                } => {
                    let _ = reply.send(file.write_all_at(&data, offset));
                }
                Job::Sync { file, reply } => {
                    let _ = reply.send(file.sync_data());
                }
            }
        }
    }
}

pub fn default_worker_threads() -> usize {
    let cores = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (cores / 2).clamp(2, 8)
}

impl DiskBackend for PreadBackend {
    fn pread(
        &self,
        file: Arc<File>,
        frame: Frame,
        offset: u64,
    ) -> oneshot::Receiver<io::Result<Frame>> {
        let (reply, rx) = oneshot::channel();
        fail_job_on_full(self.tx.try_send(Job::Read {
            file,
            frame,
            offset,
            reply,
        }));
        rx
    }

    fn pwrite(
        &self,
        file: Arc<File>,
        data: bytes::Bytes,
        offset: u64,
    ) -> oneshot::Receiver<io::Result<()>> {
        let (reply, rx) = oneshot::channel();
        fail_job_on_full(self.tx.try_send(Job::Write {
            file,
            data,
            offset,
            reply,
        }));
        rx
    }

    fn fdatasync(&self, file: Arc<File>) -> oneshot::Receiver<io::Result<()>> {
        let (reply, rx) = oneshot::channel();
        fail_job_on_full(self.tx.try_send(Job::Sync { file, reply }));
        rx
    }
}

fn fail_job_on_full(result: Result<(), mpsc::TrySendError<Job>>) {
    match result {
        Ok(()) => {}
        Err(mpsc::TrySendError::Full(job)) => {
            fail_job(job, io::ErrorKind::WouldBlock, "storage backend queue full")
        }
        Err(mpsc::TrySendError::Disconnected(job)) => fail_job(
            job,
            io::ErrorKind::BrokenPipe,
            "storage backend queue closed",
        ),
    }
}

fn fail_job(job: Job, kind: io::ErrorKind, message: &'static str) {
    let error = || io::Error::new(kind, message);
    match job {
        Job::Read { reply, .. } => {
            let _ = reply.send(Err(error()));
        }
        Job::Write { reply, .. } | Job::Sync { reply, .. } => {
            let _ = reply.send(Err(error()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FramePool;

    #[test]
    fn backend_request_parses_user_values() {
        assert_eq!(BackendRequest::parse("auto"), BackendRequest::Auto);
        assert_eq!(BackendRequest::parse("pread"), BackendRequest::Pread);
        assert_eq!(BackendRequest::parse("thread-pool"), BackendRequest::Pread);
        assert_eq!(BackendRequest::parse("io_uring"), BackendRequest::Uring);
        assert_eq!(BackendRequest::parse("surprise"), BackendRequest::Auto);
    }

    #[test]
    fn forcing_pread_selects_pread() {
        let backend = SelectedDiskBackend::select(BackendRequest::Pread, 1);
        assert_eq!(backend.kind(), BackendKind::Pread);
        assert_eq!(backend.selection().requested, BackendRequest::Pread);
        assert!(!backend.supports_fixed_buffers());
        assert!(!backend.supports_registered_files());
        assert_eq!(backend.max_batch_len(), 1);
        assert_eq!(backend.fixed_buffer_len(), 0);
    }

    #[test]
    fn uring_request_has_clean_probe_fallback() {
        let backend = SelectedDiskBackend::select(BackendRequest::Uring, 1);
        assert_eq!(backend.selection().requested, BackendRequest::Uring);
        if backend.kind() == BackendKind::Pread {
            assert!(!backend.selection().reason.is_empty());
        }
    }

    #[test]
    fn uring_probe_is_diagnostic_not_panic() {
        let probe = UringBackend::probe().unwrap();
        assert!(!probe.reason.is_empty());
        if !probe.usable {
            assert!(!probe.fixed_buffers);
            assert!(!probe.registered_files);
        }
    }

    #[tokio::test]
    async fn pwrite_then_pread_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, vec![0u8; 64]).unwrap();

        let backend = PreadBackend::new(2);
        let pool = FramePool::new(1 << 20);

        let file = Arc::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap(),
        );

        backend
            .pwrite(file.clone(), bytes::Bytes::from_static(b"hello"), 8)
            .await
            .unwrap()
            .unwrap();

        let mut frame = pool.try_acquire(5).unwrap();
        frame = backend.pread(file, frame, 8).await.unwrap().unwrap();
        assert_eq!(frame.as_slice(), b"hello");
    }

    #[tokio::test]
    async fn pread_past_eof_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.bin");
        std::fs::write(&path, b"abc").unwrap();
        let backend = PreadBackend::new(1);
        let pool = FramePool::new(1 << 20);
        let file = Arc::new(File::open(&path).unwrap());
        let frame = pool.try_acquire(16).unwrap();
        let res = backend.pread(file, frame, 0).await.unwrap();
        assert!(res.is_err()); // read_exact_at past EOF
    }

    #[tokio::test]
    async fn pread_backend_queue_fails_closed_when_full() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queued.bin");
        std::fs::write(&path, vec![0u8; 64]).unwrap();
        let file = Arc::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap(),
        );
        let (tx, _rx) = mpsc::sync_channel(1);
        let backend = PreadBackend {
            tx,
            _workers: Vec::new(),
        };

        let _queued = backend.pwrite(file.clone(), bytes::Bytes::from_static(b"first"), 0);
        let rejected = backend
            .pwrite(file, bytes::Bytes::from_static(b"second"), 8)
            .await
            .unwrap()
            .unwrap_err();

        assert_eq!(rejected.kind(), io::ErrorKind::WouldBlock);
    }

    #[tokio::test]
    async fn selected_backend_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selected.bin");
        std::fs::write(&path, vec![0u8; 64]).unwrap();

        let backend = SelectedDiskBackend::select(BackendRequest::Auto, 1);
        let pool = FramePool::new(1 << 20);
        let file = Arc::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap(),
        );

        backend
            .pwrite(file.clone(), bytes::Bytes::from_static(b"backend"), 16)
            .await
            .unwrap()
            .unwrap();
        let frame = pool.try_acquire(7).unwrap();
        let frame = backend.pread(file, frame, 16).await.unwrap().unwrap();
        assert_eq!(frame.as_slice(), b"backend");
        assert!(!backend.fixed_buffer_strategy().uses_frame_pool_slots());
    }

    #[tokio::test]
    async fn forced_uring_roundtrip_when_kernel_supports_it() {
        let probe = UringBackend::probe().unwrap();
        if !probe.usable {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("uring.bin");
        std::fs::write(&path, vec![0u8; 128]).unwrap();

        let backend = SelectedDiskBackend::select(BackendRequest::Uring, 1);
        if backend.kind() != BackendKind::Uring {
            eprintln!(
                "tng_storage_backend forced_uring_skipped reason=\"{}\"",
                backend.selection().reason
            );
            return;
        }

        let pool = FramePool::new(1 << 20);
        let file = Arc::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap(),
        );

        backend
            .pwrite(file.clone(), bytes::Bytes::from_static(b"real-uring"), 32)
            .await
            .unwrap()
            .unwrap();
        backend.fdatasync(file.clone()).await.unwrap().unwrap();

        let frame = pool.try_acquire(10).unwrap();
        let frame = backend.pread(file, frame, 32).await.unwrap().unwrap();
        assert_eq!(frame.as_slice(), b"real-uring");
        assert!(
            !backend.supports_fixed_buffers() || probe.fixed_buffers,
            "backend cannot report fixed buffers when probe rejected them"
        );
        assert!(
            !backend.supports_registered_files() || probe.registered_files,
            "backend cannot report registered files when probe rejected them"
        );
        assert_eq!(backend.max_batch_len(), URING_BATCH_LIMIT);
        if backend.supports_fixed_buffers() {
            assert_eq!(backend.fixed_buffer_len(), URING_FIXED_BUFFER_LEN);
            assert_eq!(
                backend.fixed_buffer_strategy(),
                FixedBufferStrategy::WorkerCopy
            );
        } else {
            assert_eq!(
                backend.fixed_buffer_strategy(),
                FixedBufferStrategy::Disabled
            );
        }
    }

    #[test]
    fn uring_fixed_buffer_registration_budget_stays_below_common_memlock_limit() {
        assert!(URING_FIXED_BUFFER_SLOTS < URING_BATCH_LIMIT);
        assert!(URING_FIXED_BUFFER_SLOTS * URING_FIXED_BUFFER_LEN <= 4 * 1024 * 1024);
    }

    #[test]
    fn fixed_buffer_strategy_names_are_stable_for_metrics() {
        assert_eq!(FixedBufferStrategy::Disabled.as_str(), "disabled");
        assert_eq!(FixedBufferStrategy::WorkerCopy.as_str(), "worker_copy");
        assert_eq!(
            FixedBufferStrategy::FramePoolSlots.as_str(),
            "frame_pool_slots"
        );
        assert!(FixedBufferStrategy::WorkerCopy.uses_worker_copy());
        assert!(!FixedBufferStrategy::WorkerCopy.uses_frame_pool_slots());
        assert!(!FixedBufferStrategy::FramePoolSlots.uses_worker_copy());
        assert!(FixedBufferStrategy::FramePoolSlots.uses_frame_pool_slots());
    }

    #[test]
    fn uring_strategy_never_overclaims_frame_pool_slots() {
        let backend = UringBackend::try_new_with_queue_depth(1, 1);
        let Ok(backend) = backend else {
            return;
        };
        let strategy = backend.fixed_buffer_strategy();
        assert!(
            !strategy.uses_frame_pool_slots(),
            "frame_pool_slots must only be reported after the frame pool leases registered slots"
        );
        if backend.supports_fixed_buffers() {
            assert_eq!(strategy, FixedBufferStrategy::WorkerCopy);
        } else {
            assert_eq!(strategy, FixedBufferStrategy::Disabled);
        }
    }
}
