//! Disk backend abstraction.
//!
//! All positioned I/O goes through a [`DiskBackend`]. Storage NG ships the
//! [`PreadBackend`] (a dedicated, bounded blocking thread pool calling
//! `pread`/`pwrite` via positioned I/O) as the portable default. A future
//! `UringBackend` (io_uring, registered fds + fixed buffers) will implement
//! the same trait; the elevator feeds whichever backend batches.
//!
//! The pool is deliberately *separate* from Tokio's generic blocking pool
//! so disk I/O can neither starve nor be starved by unrelated
//! `spawn_blocking` work.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use tokio::sync::oneshot;

use crate::frame::Frame;

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
        let cores = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self::new((cores / 2).clamp(2, 8))
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
}
