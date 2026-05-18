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

use once_cell::sync::Lazy;

/// Default process-wide frame-pool cap when `TNG_STORAGE_FRAME_CAP_MB` is not
/// set.
pub const DEFAULT_FRAME_CAP_MB: u64 = 256;

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
    denied: Arc<AtomicU64>,
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
            denied: Arc::new(AtomicU64::new(0)),
            cap_bytes,
        }
    }

    pub fn cap_bytes(&self) -> u64 {
        self.cap_bytes
    }

    pub fn in_use_bytes(&self) -> u64 {
        self.in_use.load(Ordering::Relaxed)
    }

    pub fn denied_allocations(&self) -> u64 {
        self.denied.load(Ordering::Relaxed)
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
                self.denied.fetch_add(1, Ordering::Relaxed);
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
                guard.free[ci]
                    .pop()
                    .unwrap_or_else(|| vec![0u8; SIZE_CLASSES[ci]])
            }
            // Oversize: exact allocation, not pooled on release.
            None => vec![0u8; len],
        };
        // Present exactly `len` usable bytes regardless of class capacity.
        if buf.len() < len {
            buf.resize(len, 0);
        }
        Some(Frame {
            len,
            backing: FrameBacking::Vec { buf, class },
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

    fn release_charge(&self, len: usize) {
        self.in_use.fetch_sub(len as u64, Ordering::AcqRel);
    }
}

/// Process-wide storage frame pool shared by `StorageRuntime` and the live
/// torrent scheduler.
pub fn global_frame_pool() -> &'static FramePool {
    static GLOBAL: Lazy<FramePool> = Lazy::new(|| {
        let cap_mb = std::env::var("TNG_STORAGE_FRAME_CAP_MB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_FRAME_CAP_MB);
        FramePool::new(cap_mb.saturating_mul(1024 * 1024))
    });
    &GLOBAL
}

#[derive(Debug)]
enum FrameBacking {
    Vec { buf: Vec<u8>, class: Option<usize> },
    Registered { slot: RegisteredFrameSlot },
    Empty,
}

/// A buffer borrowed from the [`FramePool`]. Returns to the pool on drop
/// and releases its charge against the cap.
#[derive(Debug)]
pub struct Frame {
    len: usize,
    backing: FrameBacking,
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
        match &self.backing {
            FrameBacking::Vec { buf, .. } => &buf[..self.len],
            FrameBacking::Registered { slot } => slot.as_slice(self.len),
            FrameBacking::Empty => &[],
        }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        match &mut self.backing {
            FrameBacking::Vec { buf, .. } => &mut buf[..self.len],
            FrameBacking::Registered { slot } => slot.as_mut_slice(self.len),
            FrameBacking::Empty => &mut [],
        }
    }

    /// Consume this frame as immutable bytes without copying the payload.
    ///
    /// The backing allocation is not returned to the idle frame cache because
    /// ownership moves to [`bytes::Bytes`], but the in-use frame-pool charge is
    /// released before returning.
    pub fn into_bytes(mut self) -> bytes::Bytes {
        let len = self.len;
        self.len = 0;
        match std::mem::replace(&mut self.backing, FrameBacking::Empty) {
            FrameBacking::Vec { mut buf, .. } => {
                self.pool.release_charge(len);
                buf.truncate(len);
                bytes::Bytes::from(buf)
            }
            FrameBacking::Registered { slot } => {
                let bytes = bytes::Bytes::copy_from_slice(slot.as_slice(len));
                self.pool.release_charge(len);
                drop(slot);
                bytes
            }
            FrameBacking::Empty => bytes::Bytes::new(),
        }
    }

    /// Move this frame-pool charge onto a registered fixed-buffer slot.
    pub fn into_registered_slot(mut self, slot: RegisteredFrameSlot) -> Self {
        let len = self.len;
        self.len = 0;
        self.backing = FrameBacking::Empty;
        Frame {
            len,
            backing: FrameBacking::Registered { slot },
            pool: self.pool.clone(),
        }
    }

    pub fn is_registered_slot(&self) -> bool {
        matches!(self.backing, FrameBacking::Registered { .. })
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
        match std::mem::replace(&mut self.backing, FrameBacking::Empty) {
            FrameBacking::Vec { buf, class } => self.pool.release(buf, self.len, class),
            FrameBacking::Registered { slot } => {
                drop(slot);
                self.pool.release_charge(self.len);
            }
            FrameBacking::Empty => {}
        }
    }
}

/// Stable registered fixed-buffer storage that can back returned frames.
#[derive(Debug)]
pub struct RegisteredFrameSlots {
    buffers: Vec<RegisteredBuffer>,
    free: Mutex<Vec<u16>>,
}

#[derive(Debug)]
struct RegisteredBuffer {
    ptr: std::ptr::NonNull<u8>,
    len: usize,
    _buf: Box<[u8]>,
}

unsafe impl Send for RegisteredFrameSlots {}
unsafe impl Sync for RegisteredFrameSlots {}

impl RegisteredFrameSlots {
    pub fn new(slots: usize, len: usize) -> Arc<Self> {
        let buffers = (0..slots)
            .map(|_| {
                let mut buf = vec![0u8; len].into_boxed_slice();
                let ptr = std::ptr::NonNull::new(buf.as_mut_ptr()).expect("boxed slice pointer");
                RegisteredBuffer {
                    ptr,
                    len: buf.len(),
                    _buf: buf,
                }
            })
            .collect::<Vec<_>>();
        Arc::new(Self {
            buffers,
            free: Mutex::new((0..slots as u16).rev().collect()),
        })
    }

    #[cfg(target_os = "linux")]
    pub fn iovecs(&self) -> Vec<libc::iovec> {
        self.buffers
            .iter()
            .map(|buf| libc::iovec {
                iov_base: buf.ptr.as_ptr().cast(),
                iov_len: buf.len,
            })
            .collect()
    }

    pub fn acquire(self: &Arc<Self>, len: usize) -> Option<RegisteredFrameSlot> {
        let slot = {
            let mut free = self.free.lock().expect("registered frame slots poisoned");
            let slot = *free.last()?;
            if len > self.buffers[slot as usize].len {
                return None;
            }
            free.pop().expect("slot checked above")
        };
        Some(RegisteredFrameSlot {
            set: Arc::clone(self),
            slot,
        })
    }

    fn release(&self, slot: u16) {
        let mut free = self.free.lock().expect("registered frame slots poisoned");
        free.push(slot);
    }
}

#[derive(Debug)]
pub struct RegisteredFrameSlot {
    set: Arc<RegisteredFrameSlots>,
    slot: u16,
}

impl RegisteredFrameSlot {
    pub fn index(&self) -> u16 {
        self.slot
    }

    pub fn ptr_mut(&self) -> *mut u8 {
        self.set.buffers[self.slot as usize].ptr.as_ptr()
    }

    fn as_slice(&self, len: usize) -> &[u8] {
        assert!(len <= self.set.buffers[self.slot as usize].len);
        unsafe { std::slice::from_raw_parts(self.ptr_mut().cast_const(), len) }
    }

    fn as_mut_slice(&mut self, len: usize) -> &mut [u8] {
        assert!(len <= self.set.buffers[self.slot as usize].len);
        unsafe { std::slice::from_raw_parts_mut(self.ptr_mut(), len) }
    }
}

impl Drop for RegisteredFrameSlot {
    fn drop(&mut self) {
        self.set.release(self.slot);
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
        assert_eq!(pool.denied_allocations(), 1);
        drop(a);
        // Freed → now it fits.
        assert!(pool.try_acquire(8 * 1024).is_some());
        assert_eq!(pool.denied_allocations(), 1);
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

    #[test]
    fn into_bytes_releases_charge_without_copying_payload() {
        let pool = FramePool::new(1024 * 1024);
        let mut frame = pool.try_acquire(4096).unwrap();
        frame.as_mut_slice()[..5].copy_from_slice(b"hello");
        assert_eq!(pool.in_use_bytes(), 4096);

        let bytes = frame.into_bytes();

        assert_eq!(&bytes[..5], b"hello");
        assert_eq!(bytes.len(), 4096);
        assert_eq!(pool.in_use_bytes(), 0);
    }

    #[test]
    fn registered_slot_frame_keeps_charge_until_drop() {
        let pool = FramePool::new(1024 * 1024);
        let slots = RegisteredFrameSlots::new(1, 4096);
        let slot = slots.acquire(4096).unwrap();
        unsafe {
            std::slice::from_raw_parts_mut(slot.ptr_mut(), 5).copy_from_slice(b"slot!");
        }
        let frame = pool.try_acquire(4096).unwrap().into_registered_slot(slot);

        assert!(frame.is_registered_slot());
        assert_eq!(&frame.as_slice()[..5], b"slot!");
        assert_eq!(pool.in_use_bytes(), 4096);
        assert!(slots.acquire(1).is_none());

        drop(frame);

        assert_eq!(pool.in_use_bytes(), 0);
        assert!(slots.acquire(1).is_some());
    }
}
