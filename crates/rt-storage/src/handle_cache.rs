//! Open-handle cache.
//!
//! A path-keyed cache of open file descriptors. A per-cache descriptor lease
//! bounds cached files and in-flight operations together; LRU eviction plus a
//! time-based idle sweep releases unused descriptors promptly. All storage
//! caches also draw from one shared managed-storage quota; unrelated process
//! descriptors remain outside it.
//!
//! Positioned I/O (`pread`/`pwrite`) is what makes a shared fd safe: no
//! per-op `seek`, so concurrent readers/writers do not race a file cursor.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::file_handle::{DescriptorLimiter, DescriptorPermit, LeasedFile, LeasedFileHandle};
use crate::open::{file_matches_path, open_path_no_follow};

/// Whether the cached handle is read-only or read+write. A path may have
/// one of each (a reader and a writer fd) live simultaneously.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Access {
    Read,
    Write,
}

type Key = (PathBuf, Access);

/// An operation's shareable reference to an open file. Cloning this wrapper
/// keeps its cache's descriptor permit alive with the actual descriptor.
#[derive(Debug)]
pub struct OpenFile {
    file: Arc<LeasedFile>,
}

impl OpenFile {
    /// Borrow the file while retaining this operation handle.
    pub fn file(&self) -> &File {
        &self.file
    }

    pub(crate) fn leased_handle(&self) -> Arc<LeasedFileHandle> {
        Arc::new(LeasedFileHandle::new(Arc::clone(&self.file)))
    }
}

impl Drop for OpenFile {
    fn drop(&mut self) {
        self.file.notify_waiters();
    }
}

#[derive(Debug)]
struct Entry {
    handle: Arc<LeasedFile>,
    tick: u64,
    last_used: Instant,
}

#[derive(Debug)]
struct Inner {
    map: HashMap<Key, Entry>,
    /// LRU index: tick → key. Lowest tick is least-recently-used.
    lru: BTreeMap<u64, Key>,
    next_tick: u64,
}

/// Bounded LRU cache of open file handles. Cheap to clone (`Arc` inside).
#[derive(Debug, Clone)]
pub struct HandleCache {
    inner: Arc<Mutex<Inner>>,
    cap: usize,
    idle_ttl: Duration,
    descriptors: Arc<DescriptorLimiter>,
}

impl HandleCache {
    pub fn new(cap: usize, idle_ttl: Duration) -> Self {
        HandleCache {
            inner: Arc::new(Mutex::new(Inner {
                map: HashMap::new(),
                lru: BTreeMap::new(),
                next_tick: 0,
            })),
            cap,
            idle_ttl,
            descriptors: DescriptorLimiter::new(cap),
        }
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub(crate) fn active_descriptors(&self) -> usize {
        self.descriptors.active()
    }

    pub fn len(&self) -> usize {
        self.lock_inner().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock_inner(&self) -> MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                // The map and LRU are derived cache state. In-flight I/O owns
                // a separate leased handle, and every miss reopens through
                // the no-follow path, so dropping cached entries is safe.
                let mut guard = poisoned.into_inner();
                guard.map.clear();
                guard.lru.clear();
                guard.next_tick = 0;
                self.inner.clear_poison();
                guard
            }
        }
    }

    /// Get a cached handle for `path`, opening (and inserting) one on miss.
    ///
    /// `write` selects a read+write handle (optionally `create`ing the
    /// file); otherwise a read-only handle that never creates. The open
    /// syscall runs outside the lock; a concurrent opener racing the same
    /// key is resolved by keeping whichever landed first.
    pub fn get_or_open(&self, path: &Path, write: bool, create: bool) -> io::Result<Arc<OpenFile>> {
        let key = Self::cache_key(path, write)?;
        if let Some(file) = self.touch(&key) {
            return Ok(Self::operation_handle(file));
        }

        // Reserve capacity before opening so concurrent misses cannot exceed
        // this cache's live-descriptor budget while the opens race.
        let permit = self.acquire_descriptor();
        self.open_and_insert(key, write, create, permit)
    }

    /// Async counterpart to [`HandleCache::get_or_open`]. Descriptor
    /// backpressure awaits a notification instead of blocking an executor
    /// worker while another operation owns the final lease.
    pub async fn get_or_open_async(
        &self,
        path: &Path,
        write: bool,
        create: bool,
    ) -> io::Result<Arc<OpenFile>> {
        let key = Self::cache_key(path, write)?;
        if let Some(file) = self.touch(&key) {
            return Ok(Self::operation_handle(file));
        }

        let permit = self.acquire_descriptor_async().await;
        self.open_and_insert(key, write, create, permit)
    }

    fn cache_key(path: &Path, write: bool) -> io::Result<Key> {
        // Keep the public cache API usable with relative paths while making
        // the cache key and secured open operation agree on one spelling.
        // Relative paths are resolved against the process directory, never
        // canonicalized through symlinks.
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let access = if write { Access::Write } else { Access::Read };
        Ok((path, access))
    }

    fn open_and_insert(
        &self,
        key: Key,
        write: bool,
        create: bool,
        permit: DescriptorPermit,
    ) -> io::Result<Arc<OpenFile>> {
        // Miss: open without holding the cache lock. On Unix this walks from `/`
        // through directory descriptors with O_NOFOLLOW, so a replaced
        // ancestor cannot redirect peer I/O outside the already-authorized
        // path. The final component is no-follow as well.
        let file = open_path_no_follow(&key.0, write, create)?;
        let file = Arc::new(LeasedFile::new(file, permit));

        let mut inner = self.lock_inner();
        // Another thread may have inserted the same key meanwhile.
        if let Some(existing) = inner.map.get(&key) {
            if file_matches_path(&existing.handle, &key.0).unwrap_or(false) {
                return Ok(Self::operation_handle(Arc::clone(&existing.handle)));
            }
        }
        if let Some(existing) = inner.map.remove(&key) {
            inner.lru.remove(&existing.tick);
        }
        let tick = inner.next_tick;
        inner.next_tick += 1;
        inner.lru.insert(tick, key.clone());
        inner.map.insert(
            key,
            Entry {
                handle: Arc::clone(&file),
                tick,
                last_used: Instant::now(),
            },
        );
        Self::evict_to_cap(&mut inner, self.cap);
        Ok(Self::operation_handle(file))
    }

    fn operation_handle(file: Arc<LeasedFile>) -> Arc<OpenFile> {
        Arc::new(OpenFile { file })
    }

    fn touch(&self, key: &Key) -> Option<Arc<LeasedFile>> {
        let mut inner = self.lock_inner();
        let current = inner
            .map
            .get(key)
            .is_some_and(|entry| file_matches_path(&entry.handle, &key.0).unwrap_or(false));
        if !current {
            if let Some(entry) = inner.map.remove(key) {
                inner.lru.remove(&entry.tick);
            }
            return None;
        }
        let new_tick = inner.next_tick;
        let entry = inner.map.get_mut(key)?;
        let old_tick = entry.tick;
        entry.tick = new_tick;
        entry.last_used = Instant::now();
        let handle = Arc::clone(&entry.handle);
        inner.next_tick += 1;
        inner.lru.remove(&old_tick);
        inner.lru.insert(new_tick, key.clone());
        Some(handle)
    }

    /// Close handles untouched for longer than the idle TTL. Returns the
    /// number removed from the cache; active operation leases keep their file
    /// descriptor open until backend ownership ends.
    pub fn sweep_idle(&self) -> usize {
        let now = Instant::now();
        let mut inner = self.lock_inner();
        let stale: Vec<Key> = inner
            .map
            .iter()
            .filter(|(_, e)| now.duration_since(e.last_used) >= self.idle_ttl)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &stale {
            if let Some(e) = inner.map.remove(k) {
                inner.lru.remove(&e.tick);
            }
        }
        stale.len()
    }

    fn evict_to_cap(inner: &mut Inner, cap: usize) {
        while inner.map.len() > cap {
            if !Self::evict_oldest(inner) {
                break;
            }
        }
    }

    fn evict_oldest(inner: &mut Inner) -> bool {
        let Some((&tick, _)) = inner.lru.iter().next() else {
            return false;
        };
        let key = inner
            .lru
            .remove(&tick)
            .expect("lru key present for iterated tick");
        inner.map.remove(&key);
        true
    }

    fn acquire_descriptor(&self) -> DescriptorPermit {
        loop {
            if let Some(permit) = self.descriptors.try_acquire() {
                return permit;
            }

            let generation = self.descriptors.generation();
            {
                let mut inner = self.lock_inner();
                if let Some(permit) = self.descriptors.try_acquire() {
                    return permit;
                }
                // Evicting an in-flight entry may not free its descriptor
                // yet. Operation/backend handle drops wake us to evict again
                // once that file becomes idle.
                Self::evict_oldest(&mut inner);
            }
            self.descriptors.wait_for_change(generation);
        }
    }

    async fn acquire_descriptor_async(&self) -> DescriptorPermit {
        loop {
            if let Some(permit) = self.descriptors.try_acquire() {
                return permit;
            }

            let generation = self.descriptors.generation();
            {
                let mut inner = self.lock_inner();
                if let Some(permit) = self.descriptors.try_acquire() {
                    return permit;
                }
                Self::evict_oldest(&mut inner);
            }
            self.descriptors.wait_for_change_async(generation).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn reuses_same_handle_for_repeated_opens() {
        let dir = tempfile::tempdir().unwrap();
        let p = tmp_file(dir.path(), "a.bin", b"data");
        let cache = HandleCache::new(16, Duration::from_secs(30));

        let h1 = cache.get_or_open(&p, false, false).unwrap();
        let h2 = cache.get_or_open(&p, false, false).unwrap();
        assert!(Arc::ptr_eq(&h1.file, &h2.file));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.descriptors.active(), 1);
    }

    #[test]
    fn reopens_path_after_unlink_and_recreate() {
        let dir = tempfile::tempdir().unwrap();
        let p = tmp_file(dir.path(), "replaced.bin", b"old");
        let cache = HandleCache::new(16, Duration::from_secs(30));

        let old = cache.get_or_open(&p, false, false).unwrap();
        std::fs::remove_file(&p).unwrap();
        std::fs::write(&p, b"new").unwrap();

        let new = cache.get_or_open(&p, false, false).unwrap();

        assert!(!Arc::ptr_eq(&old.file, &new.file));
    }

    #[test]
    fn read_and_write_handles_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let p = tmp_file(dir.path(), "rw.bin", b"data");
        let cache = HandleCache::new(16, Duration::from_secs(30));

        cache.get_or_open(&p, false, false).unwrap();
        cache.get_or_open(&p, true, false).unwrap();
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn zero_capacity_returns_open_files_without_retaining_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_file(dir.path(), "uncached.bin", b"data");
        let cache = HandleCache::new(0, Duration::from_secs(30));

        let handle = cache.get_or_open(&path, false, false).unwrap();

        assert_eq!(cache.capacity(), 0);
        assert_eq!(cache.len(), 0);
        assert_eq!(handle.file().metadata().unwrap().len(), 4);
        assert_eq!(cache.descriptors.active(), 1);
        drop(handle);
        assert_eq!(cache.descriptors.active(), 0);
    }

    #[test]
    fn waits_for_an_evicted_in_flight_descriptor_lease() {
        let dir = tempfile::tempdir().unwrap();
        let first_path = tmp_file(dir.path(), "first.bin", b"first");
        let second_path = tmp_file(dir.path(), "second.bin", b"second");
        let cache = Arc::new(HandleCache::new(1, Duration::from_secs(30)));
        let first = cache.get_or_open(&first_path, false, false).unwrap();
        assert_eq!(cache.descriptors.active(), 1);

        let worker_cache = Arc::clone(&cache);
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
        let (opened_tx, opened_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let second = worker_cache
                .get_or_open(&second_path, false, false)
                .unwrap();
            opened_tx.send(second).unwrap();
        });

        started_rx.recv().unwrap();
        assert!(opened_rx.recv_timeout(Duration::from_millis(50)).is_err());
        drop(first);

        let second = opened_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("waiter should open after the evicted operation releases its lease");
        assert_eq!(second.file().metadata().unwrap().len(), 6);
        assert_eq!(cache.descriptors.active(), 1);
        worker.join().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_descriptor_wait_does_not_block_executor_progress() {
        let dir = tempfile::tempdir().unwrap();
        let first_path = tmp_file(dir.path(), "async-first.bin", b"first");
        let second_path = tmp_file(dir.path(), "async-second.bin", b"second");
        let cache = Arc::new(HandleCache::new(1, Duration::from_secs(30)));
        let first = cache
            .get_or_open_async(&first_path, false, false)
            .await
            .unwrap();

        let waiter_cache = Arc::clone(&cache);
        let waiter = tokio::spawn(async move {
            waiter_cache
                .get_or_open_async(&second_path, false, false)
                .await
        });
        tokio::task::yield_now().await;

        let progressed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progressed_task = Arc::clone(&progressed);
        tokio::spawn(async move {
            progressed_task.store(true, Ordering::Release);
        });
        tokio::task::yield_now().await;
        assert!(progressed.load(Ordering::Acquire));

        drop(first);
        let second = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("async descriptor waiter should wake")
            .unwrap()
            .unwrap();
        assert_eq!(second.file().metadata().unwrap().len(), 6);
        assert_eq!(cache.descriptors.active(), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn concurrent_cache_opens_stay_within_low_descriptor_limit() {
        const CHILD_ENV: &str = "TNG_TEST_HANDLE_CACHE_CONCURRENT_LOW_FD_CHILD";

        if std::env::var_os(CHILD_ENV).is_some() {
            let mut current = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: `current` is a valid writable `rlimit` for getrlimit.
            assert_eq!(
                unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut current) },
                0
            );
            let limit = current.rlim_max.clamp(1, 64);
            let constrained = libc::rlimit {
                rlim_cur: limit,
                rlim_max: limit,
            };
            // SAFETY: lowering this child process's limits cannot affect the
            // parent test process.
            assert_eq!(
                unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &constrained) },
                0
            );

            const OPERATIONS: usize = 72;
            const ACTIVE_LIMIT: usize = 8;
            let dir = tempfile::tempdir().unwrap();
            let paths = (0..OPERATIONS)
                .map(|index| {
                    let path = dir.path().join(format!("runtime-{index}.bin"));
                    std::fs::write(&path, b"payload").unwrap();
                    path
                })
                .collect::<Vec<_>>();
            let cache = Arc::new(HandleCache::new(ACTIVE_LIMIT, Duration::from_secs(30)));
            let start = Arc::new(std::sync::Barrier::new(OPERATIONS));
            let max_active = Arc::new(AtomicUsize::new(0));
            let workers = paths
                .into_iter()
                .map(|path| {
                    let cache = Arc::clone(&cache);
                    let start = Arc::clone(&start);
                    let max_active = Arc::clone(&max_active);
                    std::thread::spawn(move || {
                        start.wait();
                        let file = cache
                            .get_or_open(&path, false, false)
                            .unwrap_or_else(|error| panic!("open {path:?} failed: {error}"));
                        max_active.fetch_max(cache.descriptors.active(), Ordering::Relaxed);
                        std::thread::sleep(Duration::from_millis(30));
                        assert_eq!(file.file().metadata().unwrap().len(), 7);
                    })
                })
                .collect::<Vec<_>>();

            for worker in workers {
                worker.join().expect("handle-cache worker panicked");
            }
            assert!(max_active.load(Ordering::Relaxed) <= ACTIVE_LIMIT);
            assert!(cache.descriptors.active() <= ACTIVE_LIMIT);
            assert!(cache.len() <= ACTIVE_LIMIT);
            return;
        }

        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "handle_cache::tests::concurrent_cache_opens_stay_within_low_descriptor_limit",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                let transcript = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                panic!("handle-cache low-fd child timed out:\n{transcript}");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let output = child.wait_with_output().unwrap();
        let transcript = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success() && transcript.contains("1 passed"),
            "low-fd child failed or did not run the test:\n{transcript}"
        );
    }

    #[test]
    fn missing_file_read_errors_and_is_not_cached() {
        let dir = tempfile::tempdir().unwrap();
        let cache = HandleCache::new(16, Duration::from_secs(30));
        let res = cache.get_or_open(&dir.path().join("nope.bin"), false, false);
        assert_eq!(res.err().map(|e| e.kind()), Some(io::ErrorKind::NotFound));
        assert_eq!(cache.len(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn final_component_symlink_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let target = tmp_file(dir.path(), "target.bin", b"secret");
        let link = dir.path().join("link.bin");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let cache = HandleCache::new(16, Duration::from_secs(30));

        let error = cache.get_or_open(&link, false, false).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
        assert!(cache.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn ancestor_symlink_is_rejected_before_opening_the_file() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(outside.path(), &alias).unwrap();
        let path = alias.join("payload.bin");
        std::fs::write(outside.path().join("payload.bin"), b"secret").unwrap();
        let cache = HandleCache::new(16, Duration::from_secs(30));

        let error = cache.get_or_open(&path, false, false).unwrap_err();
        assert!(matches!(
            error.raw_os_error(),
            Some(libc::ELOOP | libc::ENOTDIR)
        ));
        assert!(cache.is_empty());
    }

    #[test]
    fn lru_evicts_least_recently_used() {
        let dir = tempfile::tempdir().unwrap();
        let cache = HandleCache::new(2, Duration::from_secs(30));
        let a = tmp_file(dir.path(), "a", b"a");
        let b = tmp_file(dir.path(), "b", b"b");
        let c = tmp_file(dir.path(), "c", b"c");

        cache.get_or_open(&a, false, false).unwrap();
        cache.get_or_open(&b, false, false).unwrap();
        // Touch `a` so `b` becomes LRU.
        cache.get_or_open(&a, false, false).unwrap();
        cache.get_or_open(&c, false, false).unwrap(); // evicts `b`
        assert_eq!(cache.len(), 2);

        // `b` was evicted → reopening it is a fresh insert (still len 2,
        // now `a` is LRU and gets evicted).
        cache.get_or_open(&b, false, false).unwrap();
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn idle_sweep_closes_stale_handles() {
        let dir = tempfile::tempdir().unwrap();
        let p = tmp_file(dir.path(), "idle.bin", b"x");
        let cache = HandleCache::new(16, Duration::from_millis(20));
        cache.get_or_open(&p, false, false).unwrap();
        assert_eq!(cache.len(), 1);
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(cache.sweep_idle(), 1);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn poisoned_handle_cache_discards_lru_and_reopens_safely() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_file(dir.path(), "poisoned.bin", b"data");
        let cache = HandleCache::new(16, Duration::from_secs(30));
        let in_flight = cache.get_or_open(&path, false, false).unwrap();

        let inner = Arc::clone(&cache.inner);
        assert!(std::thread::spawn(move || {
            let _guard = inner.lock().unwrap();
            panic!("poison the derived file-handle cache");
        })
        .join()
        .is_err());
        assert!(cache.inner.is_poisoned());

        assert_eq!(cache.len(), 0);
        assert!(!cache.inner.is_poisoned());
        assert_eq!(in_flight.file().metadata().unwrap().len(), 4);

        let reopened = cache.get_or_open(&path, false, false).unwrap();
        assert!(!Arc::ptr_eq(&in_flight.file, &reopened.file));
        assert_eq!(cache.len(), 1);
    }
}
