//! Global bounded frame pool.
//!
//! Every read buffer and write-aggregation buffer in Storage NG is drawn
//! from one process-wide pool with a hard byte cap. RAM is therefore
//! `O(in-flight bytes)`, not `O(torrents)`: 100k idle torrents allocate
//! zero frames. When the cap is reached, acquisition fails and the caller
//! applies backpressure (the peer layer defers `unchoke`/`request`)
//! instead of growing memory unbounded.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

/// Pooled size classes. A request rounds up to the smallest class that
/// fits; requests larger than the biggest class get an exact-size,
/// non-pooled allocation (still counted against the cap).
const SIZE_CLASSES: [usize; 3] = [16 * 1024, 64 * 1024, 256 * 1024];

/// Per-class cap on retained (idle) buffers, to bound resident memory when
/// load drops. Excess freed buffers are dropped rather than retained.
const MAX_RETAINED_PER_CLASS: usize = 256;

#[derive(Debug)]
struct PoolInner {
    /// Free lists, one per size class, parallel to `SIZE_CLASSES`.
    free: [Vec<Vec<u8>>; SIZE_CLASSES.len()],
}

/// Process-wide bounded buffer pool. Cheap to clone (`Arc` inside).
#[derive(Debug, Clone)]
pub struct FramePool {
    inner: Arc<Mutex<PoolInner>>,
    in_use: Arc<AtomicU64>,
    cap_bytes: u64,
}

impl FramePool {
    /// Create a pool with a hard cap on simultaneously-acquired bytes.
    pub fn new(cap_bytes: u64) -> Self {
        FramePool {
            inner: Arc::new(Mutex::new(PoolInner {
                free: Default::default(),
            })),
            in_use: Arc::new(AtomicU64::new(0)),
            cap_bytes,
        }
    }

    pub fn cap_bytes(&self) -> u64 {
        self.cap_bytes
    }

    pub fn in_use_bytes(&self) -> u64 {
        self.in_use.load(Ordering::Relaxed)
    }

    fn class_for(len: usize) -> Option<usize> {
        SIZE_CLASSES.iter().position(|&c| len <= c)
    }

    /// Acquire a frame of at least `len` bytes, or `None` if granting it
    /// would exceed the cap. The returned frame's usable length is exactly
    /// `len`; backing capacity may be larger (a size class).
    pub fn try_acquire(&self, len: usize) -> Option<Frame> {
        let charge = len as u64;
        // Reserve against the cap first (CAS loop) so concurrent callers
        // cannot collectively overshoot.
        loop {
            let cur = self.in_use.load(Ordering::Acquire);
            let next = cur.checked_add(charge)?;
            if next > self.cap_bytes {
                return None;
            }
            if self
                .in_use
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }

        let class = Self::class_for(len);
        let mut buf = match class {
            Some(ci) => {
                let mut guard = self.inner.lock().expect("frame pool poisoned");
                guard.free[ci].pop().unwrap_or_else(|| vec![0u8; SIZE_CLASSES[ci]])
            }
            // Oversize: exact allocation, not pooled on release.
            None => vec![0u8; len],
        };
        // Present exactly `len` usable bytes regardless of class capacity.
        if buf.len() < len {
            buf.resize(len, 0);
        }
        Some(Frame {
            buf,
            len,
            class,
            pool: self.clone(),
        })
    }

    fn release(&self, mut buf: Vec<u8>, len: usize, class: Option<usize>) {
        self.in_use.fetch_sub(len as u64, Ordering::AcqRel);
        if let Some(ci) = class {
            // Restore full class capacity and retain for reuse, bounded.
            buf.clear();
            buf.resize(SIZE_CLASSES[ci], 0);
            let mut guard = self.inner.lock().expect("frame pool poisoned");
            if guard.free[ci].len() < MAX_RETAINED_PER_CLASS {
                guard.free[ci].push(buf);
            }
        }
        // Oversize buffers are simply dropped.
    }
}

/// A buffer borrowed from the [`FramePool`]. Returns to the pool on drop
/// and releases its charge against the cap.
#[derive(Debug)]
pub struct Frame {
    buf: Vec<u8>,
    len: usize,
    class: Option<usize>,
    pool: FramePool,
}

impl Frame {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.buf[..self.len]
    }
}

impl std::ops::Deref for Frame {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl std::ops::DerefMut for Frame {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.as_mut_slice()
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        let buf = std::mem::take(&mut self.buf);
        self.pool.release(buf, self.len, self.class);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_release_roundtrips_capacity() {
        let pool = FramePool::new(1024 * 1024);
        assert_eq!(pool.in_use_bytes(), 0);
        let f = pool.try_acquire(4096).unwrap();
        assert_eq!(f.len(), 4096);
        assert_eq!(pool.in_use_bytes(), 4096);
        drop(f);
        assert_eq!(pool.in_use_bytes(), 0);
    }

    #[test]
    fn cap_enforced_with_backpressure() {
        let pool = FramePool::new(20 * 1024);
        let a = pool.try_acquire(16 * 1024).unwrap();
        // 16K taken, 20K cap → an 8K request must fail (no overshoot).
        assert!(pool.try_acquire(8 * 1024).is_none());
        drop(a);
        // Freed → now it fits.
        assert!(pool.try_acquire(8 * 1024).is_some());
    }

    #[test]
    fn buffers_are_reused_within_class() {
        let pool = FramePool::new(1024 * 1024);
        let mut f = pool.try_acquire(16 * 1024).unwrap();
        f.as_mut_slice()[0] = 0xAB;
        drop(f);
        // Same class request should pop the retained buffer (zeroed).
        let f2 = pool.try_acquire(1000).unwrap();
        assert_eq!(f2.len(), 1000);
        assert_eq!(f2.as_slice()[0], 0);
    }

    #[test]
    fn oversize_is_exact_and_counted() {
        let big = 512 * 1024;
        let pool = FramePool::new(2 * big as u64);
        let f = pool.try_acquire(big).unwrap();
        assert_eq!(f.len(), big);
        assert_eq!(pool.in_use_bytes(), big as u64);
        drop(f);
        assert_eq!(pool.in_use_bytes(), 0);
    }
}
