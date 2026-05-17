# Storage NG — Next-Generation Disk I/O Design

This document specifies the next-generation disk I/O subsystem for the native
Rust engine. It supersedes the implementation behavior of `crates/rt-storage`
(`scheduler.rs`, `verify.rs`) while preserving its public surface
(`MountScheduler`, `IoClass`, `SchedulerConfig`, `PieceVerifier`,
`scheduled_read`, `scheduled_write`) as a compatibility shim during migration.

It is a design target, not current behavior. See
`docs/ENGINE.md` §"Storage model" for the original principle and
`memory`/`storage-io-gaps` for the gap analysis that motivated this.

---

## Goal

Service **tens to hundreds of thousands of torrents** and **200+ TB** on
**low CPU/RAM/fd budgets**, on **rotational disks and pooled filesystems**
(mergerfs / SnapRAID / ZFS), and beat mainstream clients
(libtorrent/qBittorrent/Transmission/rTorrent) on exactly that workload —
not match them.

### Workload truths this design is built around

1. The library is **long-tail idle**: at any instant ~1–3% of torrents have a
   connected peer. The other 97%+ must be *ready* but cost ~nothing.
2. The box is **seeding-dominated**: the hot path is random reads of small
   blocks (16 KiB) scattered across huge files, driven by remote peers.
3. The disk is **HDD / pooled**: seek latency dominates. The entire game is
   turning random access into sequential sweeps.
4. The real bottlenecks are **fd exhaustion, per-torrent fixed overhead,
   page-cache pollution, and random-seek thrash** — never SHA throughput or
   raw disk bandwidth.

### Why mainstream clients cannot follow

libtorrent, qBittorrent, Transmission, and rTorrent all schedule disk I/O
**per torrent** (per-torrent storage object / disk threads). They structurally
cannot reorder I/O *across* torrents because no component sees the other
torrents' requests. Our advantage is to make scheduling **global and
workload-aware**.

---

## Two structural bets

### Bet 1 — Global, BitTorrent-aware I/O elevator per physical device

Today `MountScheduler` is per-mount semaphores: it bounds *how many*
concurrent ops, never *which order*. At 100k torrents there are always
hundreds of outstanding peer-read requests against the same spindle. The
elevator holds them in a per-device deadline queue keyed by
`(device, file, offset)` and, within a small latency budget, **sorts and
merges them into near-sequential sweeps**.

We can reorder freely because we know what the kernel cannot:

- BitTorrent block reads have **no ordering requirement**.
- We know piece boundaries (`PieceMap` / `FileRegion`).
- We know which requesting peer is choked / uninterested (defer or drop).
- We know which torrent is cold (lowest priority).

On HDD this is the difference between ~1 MB/s of 16 KiB random reads and
~100 MB/s of merged sweeps. This is the headline differentiator.

### Bet 2 — Tiered torrents with near-zero idle cost

The scalability ceiling is not I/O; it is that `torrent_task.rs` spawns one
Tokio task + mpsc channel + timer set **per torrent** (`TorrentTask::run`).
100k of those is gigabytes of fixed overhead before a byte moves. Formalize
three tiers (orthogonal to `TorrentState`, which stays as-is):

| Tier | Trigger | Holds | Driven by |
|---|---|---|---|
| **Dormant** | no connected peers | piece bitmap (mmap-backed/compressed) + tracker deadline | one shared timer-wheel reactor for *all* dormant torrents |
| **Warm** | recent activity / peer churn | metadata in RAM, **0 fds, 0 frames** | shared reactor |
| **Hot** | ≥1 connected peer | fds from handle cache, frames from global pool, own task | dedicated task (today's `TorrentTask`) |

A `Dormant` torrent is a few hundred bytes plus a slot in a timer wheel — not
a task, not a channel, not an fd. Promotion to `Hot` on inbound peer/announce
is microseconds. This is what makes "hundreds of thousands" real; the rest of
this document assumes it.

`TorrentState` (`rt-session`) is unchanged: `Seeding` and `Downloading`
torrents are `Dormant`/`Warm` until a peer connects, then `Hot`.

---

## Layered architecture

```
peer read/write requests  (Hot torrents only)
        │
        ▼
┌────────────────────────────────────────────────┐
│ 1. Global bounded frame pool                     │  fixed slabs (16K/64K/256K),
│    RAM = O(in-flight bytes), not O(torrents)     │  hard cap → peer backpressure
├────────────────────────────────────────────────┤
│ 2. Per-device elevator + deadline scheduler      │  sort/merge/coalesce by offset,
│    class- AND geometry-aware                     │  age-bounded, choke-aware
├────────────────────────────────────────────────┤
│ 3. Open-handle cache (workload-tiered LRU)       │  fds for Hot torrents only,
│    pread/pwrite, shareable, fadvise-driven       │  idle sweep, rlimit-bounded
├────────────────────────────────────────────────┤
│ 4. Disk backend trait                            │  io_uring (Linux) │ pread
│    registered fds + fixed buffers (uring)        │  threadpool fallback
└────────────────────────────────────────────────┘
        │
   physical device topology (resolved once per storage root)
```

`MountScheduler::acquire()` keeps returning a permit, but the permit now
carries an elevator submission handle rather than gating a bare `tokio::fs`
open. `scheduled_read`/`scheduled_write` become thin wrappers that submit to
the elevator and await completion, so `rt-engine` call sites
(`write_block`, `read_upload_block`, `PieceVerifier`) need no signature
change in phase 1.

### 1. Global frame pool

A process-wide slab allocator of fixed-size frames in a few size classes
(16 KiB, 64 KiB, 256 KiB). All read buffers and write-aggregation buffers come
from it. A hard byte cap means RAM is `O(active transfer)`; when exhausted,
read/write submission returns `StorageError::QueueFull` and the peer layer
applies backpressure (stop sending `unchoke` / defer `request`). 100k idle
torrents allocate zero frames.

```rust
pub struct FramePool { /* per-class free lists, atomic byte counter, cap */ }
pub struct Frame { /* aligned for O_DIRECT/io_uring fixed buffers */ }

impl FramePool {
    pub fn try_acquire(&self, len: usize) -> Option<Frame>; // None = at cap
    pub fn cap_bytes(&self) -> u64;
    pub fn in_use_bytes(&self) -> u64;
}
```

The current `io_uring` backend can return reads from registered frame slots
when the kernel accepts fixed buffers. Those slot leases keep the frame-pool
charge until the caller drops the returned frame; compatibility `Bytes`
conversion copies only when it must return a registered slot lease.

### 2. Per-device elevator

One elevator per **physical device** (not per mount path — see §Topology).
Each submitted op is `IoOp { device, file_key, offset, len, class, deadline,
choke_critical }`. A device worker drains its queue on a short cadence:

1. Bucket pending ops by `file_key`, sort by `offset`.
2. Merge adjacent/overlapping reads into one backend op (coalesced read,
   then scatter results back to waiters).
3. Emit in elevator (ascending-offset sweep) order, honoring per-class
   weights from `IoClass` and the existing HDD/SSD concurrency from
   `SchedulerConfig`.
4. Any op past its `deadline`, or marked `choke_critical` (a block owed to a
   peer we are about to unchoke), is promoted to the front.

Latency budget: configurable, default ~5–15 ms on `Hdd`/`Network`, ~0 on
`Ssd`/`Nvme` (elevator degenerates to pass-through — no benefit reordering
flash). `IoClass::Foreground` bypasses the budget entirely.

```rust
pub struct DeviceElevator {
    device: DeviceId,
    profile: StorageProfile,        // from rt_path::StorageProfile
    budget: Duration,
    queue: BinaryHeap<IoOp>,        // ordered by (deadline, offset)
}

pub struct IoOp {
    pub file_key: FileKey,          // (StorageRootId, SafeRelPath) → handle cache
    pub offset: u64,
    pub len: u32,
    pub class: IoClass,             // existing rt_storage::IoClass
    pub kind: IoKind,               // Read | Write(Frame)
    pub deadline: Instant,
    pub choke_critical: bool,
    pub completion: oneshot::Sender<Result<Frame, StorageError>>,
}
```

Cross-torrent reordering is the property no per-torrent client has: the heap
contains ops from every `Hot` torrent on that spindle at once.

### 3. Open-handle cache

Path-keyed LRU of open file descriptors. Capacity is a fraction of
`RLIMIT_NOFILE` (queried via `nix::sys::resource::getrlimit`, raised toward
the hard limit at startup), minus a reserve for sockets. Eviction is LRU
**plus a time-based idle sweep**: a handle unused for `idle_ttl` (default
30 s) is closed even below capacity, so a torrent going Dormant releases its
fds promptly. Handles are only ever created for `Hot` torrents.

All I/O is **positioned** (`pread`/`pwrite` via `FileExt::read_at` /
io_uring), so a single cached fd is safely shared by concurrent ops with no
`seek` and no per-op open/close — this is the property that makes the cache
usable and is why `seek`-based `scheduled_read` must go.

```rust
pub struct HandleCache {
    map: Mutex<LruMap<FileKey, Arc<OpenFile>>>,
    cap: usize,                 // ≈ rlimit_nofile * 0.8 - socket_reserve
    idle_ttl: Duration,
}
pub struct OpenFile { fd: RawFd, last_used: AtomicInstant /* + fadvise state */ }
```

"Too many open files" — rTorrent's classic failure — becomes structurally
impossible: fd count is bounded by `cap` regardless of torrent count.

### 4. Disk backend trait

```rust
#[async_trait]
pub trait DiskBackend: Send + Sync {
    async fn pread (&self, fd: RawFd, buf: Frame, off: u64) -> io::Result<Frame>;
    async fn pwrite(&self, fd: RawFd, buf: Frame, off: u64) -> io::Result<()>;
    async fn fdatasync(&self, fd: RawFd) -> io::Result<()>;
    fn supports_fixed_buffers(&self) -> bool;
}
```

- **`UringBackend`** (Linux ≥ 5.6): registered fds + registered fixed
  buffers + batched submit/complete. One or two backend threads service
  hundreds of thousands of in-flight ops. `unsafe` is confined to the
  io_uring dependency crate (consistent with the "no unsafe except in deps"
  convention).
- **`PreadBackend`** (fallback: old kernels, containers without io_uring,
  non-Linux): dedicated bounded blocking threadpool calling `pread`/`pwrite`.
  This is *separate* from Tokio's generic blocking pool so disk I/O cannot
  starve, and starves nothing.

Backend is selected at startup by probing; overridable via config. The
elevator feeds whichever backend *batches*, never individual `tokio::fs`
calls.

---

## Standout behaviors

### Page-cache stewardship (`posix_fadvise`)

At 100k torrents, one-shot cold reads will evict the page cache that hot
torrents depend on. Therefore:

- Detected-streaming connection → `fadvise(SEQUENTIAL | WILLNEED)` ahead.
- After serving a block for a cold/low-priority torrent →
  `fadvise(DONTNEED)` on that range so it never pollutes cache.
- Hot torrents' working sets are left resident.

No mainstream client manages the kernel cache deliberately; they all fight
it. This is high-leverage and cheap.

### Adaptive per-connection readahead

Classify each peer connection's access pattern (linear/streaming vs
rarest-first random). Random → readahead 0. Streaming → readahead up to a
piece or more. Readahead reads are injected into the elevator at low priority
so they fill seek gaps for free instead of competing. Contrast: libtorrent's
fixed 32-block (`read_cache_line_size`) cache line regardless of pattern.

### Piece-aggregated writes, zero read-after-write

Buffer the whole piece in one pooled frame; hash it **in memory** on a
hashing pool as the last block lands; write once with a single coalesced
`pwrite` (merging contiguous multi-file `FileRegion`s); free the frame. The
current `verify_piece` reopen-and-reread of just-written data
(`torrent_task.rs`) is deleted entirely. SHA1/SHA256 moves off the async
runtime onto a dedicated hashing pool (today it runs synchronously inside the
async `PieceVerifier::verify_piece`).

### Group-commit durability

Per-piece `fsync` across 100k torrents is fatal on HDD. Instead: a per-device
`fdatasync` **barrier** on a timer / byte threshold (e.g. every 5 s or
64 MiB). The durable fastresume watermark only advances past a *completed*
barrier. On crash, recheck only the bounded set of pieces written since the
last barrier, device-sequentially through the elevator — not a full-library
rescan. The fastresume file write itself stays atomic (tmp + rename, already
correct in `rt-fastresume`). Group commit is standard in databases and absent
from BitTorrent clients.

### Recheck as a planned sweep

Recheck (`PieceVerifier`) becomes a low-priority elevator producer:
device-sequential, throttled, page-cache-polite (`DONTNEED` after each
region), and `SEEK_HOLE`/`SEEK_DATA`-aware so sparse gaps are skipped instead
of read as zeros. A 100k-torrent recheck is a planned linear sweep, not
random thrash. Resumable-recheck checkpoints (already in `rt-jobs`) are
unchanged.

### Topology-aware preallocation

The topology layer resolves each storage root to its backing device and
filesystem (`/sys/block` rotational flag, `nr_requests`, dm/RAID, mergerfs
branch, ZFS/btrfs detection). Preallocation policy is derived, not a global
flag:

- Rotational, non-CoW (ext4/XFS) → `fallocate` per file at first touch
  (fragmentation is the #1 long-term HDD throughput killer).
- SSD/NVMe, or CoW (btrfs/ZFS), or network → stay sparse (preallocation is
  pointless or harmful).

`create_dir_all` is called once per file at allocation, never per block (the
current `write_block` calls it per block).

### Physical device topology

One elevator + one frame budget + one fadvise policy **per spindle/array**,
keyed by resolved `DeviceId`, not per mount path — because big seedboxes are
mergerfs/SnapRAID/ZFS pools and ordering by logical path orders by the wrong
axis. `StorageProfile` (`Hdd|Ssd|Nvme|Network|Unknown`) is auto-detected
here instead of defaulting to `Unknown` (as `engine.rs` does today).

---

## What this buys

| Property | Mainstream clients | Storage NG |
|---|---|---|
| Idle cost / torrent | task + threads + fds | bitmap + timer slot, 0 fd, 0 frame |
| RAM scaling | O(torrents) | O(hot torrents + in-flight bytes), hard cap |
| HDD seed throughput | per-torrent random | global elevator-merged sweeps |
| fd usage | grows with torrents (rTorrent: exhausts) | bounded by handle cache |
| Crash recovery | full recheck or fastresume-trust | bounded post-barrier recheck |
| Page cache | uncontrolled | actively stewarded |

---

## Migration / compatibility

Incremental; `rt-storage`'s public API is the seam.

1. **Phase A (parity floor):** handle cache + positioned I/O + frame pool +
   `PreadBackend`, behind the existing `scheduled_read`/`scheduled_write`
   signatures. No `rt-engine` changes. Removes the open/close-per-block
   storm immediately.
2. **Phase B (standout):** per-device elevator + topology detection. Ship
   early on a real HDD dataset to validate the seek win against the
   `recheck-vs-seeding` benchmark.
3. **Phase C (scale unlock):** tiered torrent model in `rt-engine`
   (Dormant/Warm/Hot + shared reactor). Largest refactor, largest payoff.
4. **Phase D (efficiency):** `UringBackend`, group-commit durability,
   adaptive readahead + fadvise, piece-aggregated writes.

Each phase is independently shippable and benchmarkable.

---

## Risks and honest tradeoffs

- **Tiered model is a real refactor** of the one-task-per-torrent assumption
  in `torrent_task.rs`. Highest value, highest risk; everything else is
  additive beneath the `MountScheduler` API.
- **Elevator adds bounded read latency** (the budget). Invisible for
  seeding; `choke_critical` + `Foreground` bypass protect the cases that
  care. Must never delay a block to a peer about to be unchoked.
- **Global scheduling couples torrents on one spindle.** Per-`DeviceId`
  isolation contains the blast radius — correct device resolution is
  load-bearing; a wrong mapping (e.g. mergerfs branch miss) degrades
  ordering, never correctness.
- **io_uring portability:** the `DiskBackend` trait and `PreadBackend` must
  exist from day one, not be retrofitted. io_uring is an optimization, not a
  requirement.
- **`fadvise(DONTNEED)` can hurt** if a "cold" torrent is about to get hot;
  drive it from the tier signal, not per-op heuristics, and never on `Hot`.

---

## Acceptance targets

Beyond the existing benchmark targets in `CLAUDE.md`:

- 100k synthetic torrents, ≤2% with active peers: idle RSS within release
  target; fd count ≤ handle-cache cap; ≤1 Tokio task per *Hot* torrent.
- HDD seed mix (100k torrents, churned peer set): aggregate read throughput
  ≥ 5× the non-elevator baseline on the same dataset.
- Kill -9 under write load: post-restart recheck bounded to pieces written
  since last barrier; zero silent corruption.
- Recheck of 50k torrents runs as a device-sequential sweep without
  starving active seeding (existing starvation benchmark, tighter bound).
