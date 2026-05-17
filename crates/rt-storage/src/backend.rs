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
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use io_uring::{opcode, types, IoUring};
use tokio::sync::oneshot;

use crate::frame::Frame;

const URING_ENTRIES: u32 = 256;
const URING_BATCH_LIMIT: usize = 64;
const URING_FILE_SLOTS: u32 = URING_ENTRIES;

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

impl SelectedDiskBackend {
    /// Select and construct a backend with an explicit worker-thread budget.
    pub fn select(requested: BackendRequest, threads: usize) -> Self {
        match requested {
            BackendRequest::Pread => Self::pread(
                requested,
                threads,
                "forced by storage backend configuration".to_string(),
            ),
            BackendRequest::Auto => Self::pread(
                requested,
                threads,
                "auto uses pread baseline; request io_uring explicitly after correctness benchmarks"
                    .to_string(),
            ),
            BackendRequest::Uring => match UringBackend::probe() {
                Ok(probe) if probe.usable => Self::uring(requested, threads, probe.reason),
                Ok(probe) => Self::pread(requested, threads, probe.reason),
                Err(reason) => Self::pread(requested, threads, reason),
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

    fn pread(requested: BackendRequest, threads: usize, reason: String) -> Self {
        let selection = BackendSelection {
            requested,
            selected: BackendKind::Pread,
            reason,
        };
        Self {
            selection,
            inner: SelectedDiskBackendInner::Pread(PreadBackend::new(threads)),
        }
    }

    fn uring(requested: BackendRequest, threads: usize, reason: String) -> Self {
        let backend = UringBackend::new(threads);
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
}

/// Probe information for the `io_uring` backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UringProbe {
    pub usable: bool,
    pub reason: String,
}

/// Linux `io_uring` backend.
pub struct UringBackend {
    tx: mpsc::Sender<Job>,
    _workers: Vec<thread::JoinHandle<()>>,
}

impl UringBackend {
    pub fn new(threads: usize) -> Self {
        let threads = threads.max(1);
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let mut workers = Vec::with_capacity(threads);
        for i in 0..threads {
            let rx = Arc::clone(&rx);
            let handle = thread::Builder::new()
                .name(format!("tng-uring-{i}"))
                .spawn(move || UringWorker::new(rx).run())
                .expect("spawn io_uring worker");
            workers.push(handle);
        }
        Self {
            tx,
            _workers: workers,
        }
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
                    reason: format!("kernel reports io_uring disabled ({disabled})"),
                });
            }
            if let Err(e) = IoUring::new(8) {
                return Ok(UringProbe {
                    usable: false,
                    reason: format!("io_uring probe failed: {e}"),
                });
            }
            return Ok(UringProbe {
                usable: true,
                reason: "io_uring probe succeeded".to_string(),
            });
        }

        #[cfg(not(target_os = "linux"))]
        {
            Ok(UringProbe {
                usable: false,
                reason: "io_uring is only available on Linux; using pread fallback".to_string(),
            })
        }
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
        let _ = self.tx.send(Job::Read {
            file,
            frame,
            offset,
            reply,
        });
        rx
    }

    fn pwrite(
        &self,
        file: Arc<File>,
        data: bytes::Bytes,
        offset: u64,
    ) -> oneshot::Receiver<io::Result<()>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Job::Write {
            file,
            data,
            offset,
            reply,
        });
        rx
    }

    fn fdatasync(&self, file: Arc<File>) -> oneshot::Receiver<io::Result<()>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Job::Sync { file, reply });
        rx
    }

    fn supports_fixed_buffers(&self) -> bool {
        // Fixed buffers require the frame pool to hand out stable registered
        // slot indexes. The backend already uses registered file slots when
        // the kernel accepts them.
        false
    }
}

struct UringWorker {
    rx: Arc<Mutex<mpsc::Receiver<Job>>>,
    ring: IoUring,
    next_id: u64,
    next_file_slot: u32,
    registered_files: bool,
    pending: HashMap<u64, PendingUring>,
}

enum PendingUring {
    Read {
        file: Arc<File>,
        file_slot: Option<u32>,
        frame: Frame,
        expected: usize,
        reply: oneshot::Sender<io::Result<Frame>>,
    },
    Write {
        file: Arc<File>,
        file_slot: Option<u32>,
        data: bytes::Bytes,
        expected: usize,
        reply: oneshot::Sender<io::Result<()>>,
    },
    Sync {
        file: Arc<File>,
        file_slot: Option<u32>,
        reply: oneshot::Sender<io::Result<()>>,
    },
}

impl UringWorker {
    fn new(rx: Arc<Mutex<mpsc::Receiver<Job>>>) -> Self {
        let ring = IoUring::new(URING_ENTRIES).expect("create io_uring");
        let registered_files = match ring.submitter().register_files_sparse(URING_FILE_SLOTS) {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!(error = %e, "io_uring fixed-file table unavailable");
                false
            }
        };
        Self {
            rx,
            ring,
            next_id: 1,
            next_file_slot: 0,
            registered_files,
            pending: HashMap::new(),
        }
    }

    fn run(mut self) {
        while let Some(first) = self.recv_job() {
            let jobs = self.recv_batch(first);
            let mut submitted = 0;
            for job in jobs {
                match self.submit_job(job) {
                    Ok(()) => submitted += 1,
                    Err(e) => tracing::warn!(error = %e, "io_uring submission failed"),
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
                let ptr = frame.as_mut_slice().as_mut_ptr();
                let len = frame.len();
                let file_slot = self.register_file_slot(file.as_raw_fd());
                let entry = match file_slot {
                    Some(slot) => opcode::Read::new(types::Fixed(slot), ptr, len as _),
                    None => opcode::Read::new(types::Fd(file.as_raw_fd()), ptr, len as _),
                }
                .offset(offset)
                .build()
                .user_data(id);
                self.push_entry(entry)?;
                self.pending.insert(
                    id,
                    PendingUring::Read {
                        file,
                        file_slot,
                        frame,
                        expected: len,
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
                let ptr = data.as_ptr();
                let len = data.len();
                let file_slot = self.register_file_slot(file.as_raw_fd());
                let entry = match file_slot {
                    Some(slot) => opcode::Write::new(types::Fixed(slot), ptr, len as _),
                    None => opcode::Write::new(types::Fd(file.as_raw_fd()), ptr, len as _),
                }
                .offset(offset)
                .build()
                .user_data(id);
                self.push_entry(entry)?;
                self.pending.insert(
                    id,
                    PendingUring::Write {
                        file,
                        file_slot,
                        data,
                        expected: len,
                        reply,
                    },
                );
            }
            Job::Sync { file, reply } => {
                let file_slot = self.register_file_slot(file.as_raw_fd());
                let entry = match file_slot {
                    Some(slot) => opcode::Fsync::new(types::Fixed(slot)),
                    None => opcode::Fsync::new(types::Fd(file.as_raw_fd())),
                }
                .flags(types::FsyncFlags::DATASYNC)
                .build()
                .user_data(id);
                self.push_entry(entry)?;
                self.pending.insert(
                    id,
                    PendingUring::Sync {
                        file,
                        file_slot,
                        reply,
                    },
                );
            }
        }
        Ok(())
    }

    fn register_file_slot(&mut self, fd: RawFd) -> Option<u32> {
        if !self.registered_files {
            return None;
        }
        let slot = self.next_file_slot;
        self.next_file_slot = (self.next_file_slot + 1) % URING_FILE_SLOTS;
        match self.ring.submitter().register_files_update(slot, &[fd]) {
            Ok(1) => Some(slot),
            Ok(_) => None,
            Err(e) => {
                tracing::debug!(slot, error = %e, "io_uring fixed-file update failed");
                None
            }
        }
    }

    fn unregister_file_slot(&self, slot: Option<u32>) {
        let Some(slot) = slot else {
            return;
        };
        let _ = self.ring.submitter().register_files_update(slot, &[-1]);
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
                    file_slot,
                    frame,
                    expected,
                    reply,
                } => {
                    self.unregister_file_slot(file_slot);
                    let _keepalive = file;
                    let _ = reply.send(match uring_result(result) {
                        Ok(n) if n == expected => Ok(frame),
                        Ok(_) => Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "short io_uring read",
                        )),
                        Err(e) => Err(e),
                    });
                }
                PendingUring::Write {
                    file,
                    file_slot,
                    data,
                    expected,
                    reply,
                } => {
                    self.unregister_file_slot(file_slot);
                    let _keepalive_file = file;
                    let _keepalive = data;
                    let _ = reply.send(match uring_result(result) {
                        Ok(n) if n == expected => Ok(()),
                        Ok(_) => Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "short io_uring write",
                        )),
                        Err(e) => Err(e),
                    });
                }
                PendingUring::Sync {
                    file,
                    file_slot,
                    reply,
                } => {
                    self.unregister_file_slot(file_slot);
                    let _keepalive = file;
                    let _ = reply.send(uring_result(result).map(|_| ()));
                }
            }
        }
    }

    fn fail_all(&mut self, err: io::Error) {
        let message = err.to_string();
        for (_, pending) in self.pending.drain() {
            let e = || io::Error::new(err.kind(), message.clone());
            match pending {
                PendingUring::Read { reply, .. } => {
                    let _ = reply.send(Err(e()));
                }
                PendingUring::Write { reply, .. } | PendingUring::Sync { reply, .. } => {
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
    tx: mpsc::Sender<Job>,
    _workers: Vec<thread::JoinHandle<()>>,
}

impl PreadBackend {
    /// Spawn `threads` dedicated I/O workers (clamped to at least 1).
    pub fn new(threads: usize) -> Self {
        let threads = threads.max(1);
        let (tx, rx) = mpsc::channel::<Job>();
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
        if self
            .tx
            .send(Job::Read {
                file,
                frame,
                offset,
                reply,
            })
            .is_err()
        {
            // Pool gone: rx will resolve to Canceled, surfaced as an error.
        }
        rx
    }

    fn pwrite(
        &self,
        file: Arc<File>,
        data: bytes::Bytes,
        offset: u64,
    ) -> oneshot::Receiver<io::Result<()>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Job::Write {
            file,
            data,
            offset,
            reply,
        });
        rx
    }

    fn fdatasync(&self, file: Arc<File>) -> oneshot::Receiver<io::Result<()>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Job::Sync { file, reply });
        rx
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
        assert_eq!(backend.kind(), BackendKind::Uring);

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
    }
}
