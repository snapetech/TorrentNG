//! Open-handle cache.
//!
//! A path-keyed cache of open file descriptors. Without it, every 16 KiB
//! peer block does `open()`+`close()` (the current behavior). With it, an
//! fd is opened once and shared by every concurrent positioned op against
//! that file. Capacity is bounded to a fraction of `RLIMIT_NOFILE`, with
//! LRU eviction plus a time-based idle sweep, so fd count is bounded
//! regardless of torrent count and a torrent going dormant releases its
//! fds promptly.
//!
//! Positioned I/O (`pread`/`pwrite`) is what makes a shared fd safe: no
//! per-op `seek`, so concurrent readers/writers do not race a file cursor.

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Whether the cached handle is read-only or read+write. A path may have
/// one of each (a reader and a writer fd) live simultaneously.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Access {
    Read,
    Write,
}

type Key = (PathBuf, Access);

/// A cached, shareable open file. `file` is safe to use concurrently
/// because all I/O through it is positioned (`pread`/`pwrite`).
#[derive(Debug)]
pub struct OpenFile {
    file: Arc<File>,
}

impl OpenFile {
    pub fn file(&self) -> Arc<File> {
        Arc::clone(&self.file)
    }
}

#[derive(Debug)]
struct Entry {
    handle: Arc<OpenFile>,
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
}

impl HandleCache {
    pub fn new(cap: usize, idle_ttl: Duration) -> Self {
        HandleCache {
            inner: Arc::new(Mutex::new(Inner {
                map: HashMap::new(),
                lru: BTreeMap::new(),
                next_tick: 0,
            })),
            cap: cap.max(1),
            idle_ttl,
        }
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("handle cache poisoned").map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get a cached handle for `path`, opening (and inserting) one on miss.
    ///
    /// `write` selects a read+write handle (optionally `create`ing the
    /// file); otherwise a read-only handle that never creates. The open
    /// syscall runs outside the lock; a concurrent opener racing the same
    /// key is resolved by keeping whichever landed first.
    pub fn get_or_open(&self, path: &Path, write: bool, create: bool) -> io::Result<Arc<OpenFile>> {
        let access = if write { Access::Write } else { Access::Read };
        let key: Key = (path.to_path_buf(), access);

        if let Some(h) = self.touch(&key) {
            return Ok(h);
        }

        // Miss: open without holding the lock.
        let file = if write {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(create)
                .truncate(false)
                .open(path)?
        } else {
            OpenOptions::new()
                .read(true)
                .write(false)
                .create(false)
                .truncate(false)
                .open(path)?
        };
        let handle = Arc::new(OpenFile {
            file: Arc::new(file),
        });

        let mut inner = self.inner.lock().expect("handle cache poisoned");
        // Another thread may have inserted the same key meanwhile.
        if let Some(existing) = inner.map.get(&key) {
            return Ok(Arc::clone(&existing.handle));
        }
        let tick = inner.next_tick;
        inner.next_tick += 1;
        inner.lru.insert(tick, key.clone());
        inner.map.insert(
            key,
            Entry {
                handle: Arc::clone(&handle),
                tick,
                last_used: Instant::now(),
            },
        );
        Self::evict_to_cap(&mut inner, self.cap);
        Ok(handle)
    }

    fn touch(&self, key: &Key) -> Option<Arc<OpenFile>> {
        let mut inner = self.inner.lock().expect("handle cache poisoned");
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
    /// number closed. The fd is only really closed once all in-flight ops
    /// (which hold their own `Arc<File>`) drop it.
    pub fn sweep_idle(&self) -> usize {
        let now = Instant::now();
        let mut inner = self.inner.lock().expect("handle cache poisoned");
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
            let Some((&tick, _)) = inner.lru.iter().next() else {
                break;
            };
            let key = inner
                .lru
                .remove(&tick)
                .expect("lru key present for iterated tick");
            inner.map.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(Arc::ptr_eq(&h1, &h2));
        assert_eq!(cache.len(), 1);
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
    fn missing_file_read_errors_and_is_not_cached() {
        let dir = tempfile::tempdir().unwrap();
        let cache = HandleCache::new(16, Duration::from_secs(30));
        let res = cache.get_or_open(&dir.path().join("nope.bin"), false, false);
        assert_eq!(res.err().map(|e| e.kind()), Some(io::ErrorKind::NotFound));
        assert_eq!(cache.len(), 0);
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
}
