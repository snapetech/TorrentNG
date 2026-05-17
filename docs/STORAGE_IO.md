# Storage I/O

This document tracks the native engine storage path for large seedboxes. The
target is explicit userspace I/O control for 10k-100k torrents and 200+ TB
libraries without mmap as the primary data path.

The executable feature matrix for this storage branch lives in
[`STORAGE_NG_TEST_MATRIX.md`](STORAGE_NG_TEST_MATRIX.md).
Open storage and memory follow-up work is tracked in
[`STORAGE_MEMORY_GAP_REGISTER.md`](STORAGE_MEMORY_GAP_REGISTER.md).
The current storage implementation map, including the split between the live
torrent hot path and the backend/runtime probe path, lives in
[`STORAGE_WORK_MAP.md`](STORAGE_WORK_MAP.md).

## Previous Gap

The original native path used a simple per-block primitive:

- `open -> seek -> read/write -> close` for every block.
- `create_dir_all` before every download write.
- No open-file cache or descriptor budget.
- No preallocation, so disk space and fragmentation failures appeared late.
- No `fdatasync`/`sync_data` checkpoint before trusting fastresume.
- Piece verification hashed synchronously after async disk reads.

That was correct enough for small tests, but it created avoidable fd churn,
Tokio blocking-pool contention, HDD fragmentation, late `ENOSPC`, weak
crash-recovery trust, and poor seed-read locality.

## References

The architecture follows the lessons from libtorrent-rasterbar's disk I/O
design: separate disk work from protocol work, keep file descriptors pooled,
bound queues and buffers, and make cache/scheduler behavior explicit. It also
intentionally avoids the libtorrent 2.x mmap direction that was later rolled
back for workloads where the client needs tighter control over disk pressure.
The preallocation requirements mirror long-standing qBittorrent and
Transmission behavior: sparse allocation is cheap and predictable for most
filesystems, while full preallocation is reserved for storage where it helps
most: rotational, non-CoW local filesystems.

## Current Implementation

`rt-storage::MountScheduler` now owns the live torrent disk layer beneath the
class semaphores:

- `StorageIoConfig` carries file-pool size, idle TTL, I/O worker count, queue
  depth, preallocation mode, durability mode, and peer-read readahead target.
  `PreallocationMode::Auto` resolves at scheduler construction time from the
  detected topology. Native `[storage]` TOML exposes these scheduler knobs so
  operators can tune them without code changes.
- `scheduled_read` and `scheduled_write` remain compatibility wrappers, but
  call positioned `read_at`/`write_at`.
- Disk syscalls are submitted through the probe-selected `DiskBackend`
  (`pread` baseline or explicit Linux `io_uring`) while `MountScheduler`
  preserves its per-class, per-mount, and per-device backpressure gates.
- SHA-1 and BEP52 leaf/root hashing run on a separate bounded hashing pool.
- The open-file pool is keyed by normalized absolute path, tracks read/write
  mode, hits, misses, evictions, idle closes, and open count.
- File-pool capacity is clamped to a conservative fraction of `RLIMIT_NOFILE`
  on Unix when the soft limit is available.
- New schedulers can auto-detect HDD, SSD/NVMe, or network profiles from Linux
  mount and sysfs topology when callers do not override the storage profile.
  The same topology read records a stable `DeviceId`, filesystem type, and
  whether the mount is likely CoW. Local device ids come from `/sys/dev/block`
  parent block devices; network mounts use the mount source.
- Reads open read-only with `create(false)` and never create or truncate files.
- Writes use positioned I/O and validate short writes.
- `prepare_file` creates parents and applies `PreallocationMode::{Off, Sparse,
  Full}` before first write. `Auto` is resolved to `Full` only for rotational
  non-CoW local storage; SSD/NVMe, network, unknown, and CoW filesystems stay
  sparse.
- `sync_data` and `sync_all_open_files` provide durability checkpoints.
- `DurabilityMode::Strict` syncs after writes; `Checkpoint` syncs open torrent
  files before clean fastresume saves; `Fast` preserves older relaxed behavior.
- `StorageIoStats` exposes file-pool counters, queue depths, dirty file count,
  bytes and operations by `IoClass`, sync count, hash count, and preallocation
  fallback/failure counters. Queued disk bytes are actual short-lived payload
  leases held by queued or active disk, hash, and peer-read elevator jobs, not
  queue-depth estimates.
- The live torrent hot path and `rt-storage::StorageRuntime` share the same
  probe-selected backend layer:
  `TNG_STORAGE_BACKEND=auto|pread|uring` chooses between the portable
  positioned-I/O worker pool and Linux `io_uring` positioned reads, writes,
  and data sync. `auto` currently selects the conservative `pread` baseline;
  `uring` must be requested explicitly while correctness and hardware
  benchmarks mature. Both backend implementations use bounded internal queues
  and fail closed when saturated. The uring worker keeps a stable per-worker
  fixed-file table keyed by file identity and registers worker-owned fixed
  buffers when the kernel accepts them. Kernels or
  containers that reject `io_uring` fall back to `pread` with an explicit
  diagnostic reason instead of silently changing behavior. Direct backend
  probes remain available for hardware certification and capability metrics.
- `IoClass::PeerRead` uses a small internal readahead cache when configured:
  the backend may read ahead within the same file, but callers receive exactly
  the requested byte range.
- `DeviceElevator` now exists as a self-contained per-device scheduling policy:
  HDD/network queues can hold work for a short budget, dispatch sorted by file
  offset, coalesce adjacent reads, and promote deadline-expired,
  foreground, or choke-critical work. HDD peer reads are wired through the
  elevator when `peer_read_elevator_budget_ms` is non-zero, and metrics expose
  enablement, queue depth, backend batches, and coalesced logical requests.
- Schedulers that resolve to the same storage device share a process-level
  device queue semaphore before submitting positioned disk work, so separate
  mounts on the same spindle cannot each consume a full independent disk queue.

`TorrentTask` keeps a per-file preparation registry so parent directories and
file allocation are no longer in the per-block hot path. It also keeps
in-memory piece assembly buffers for active downloads, so completed-piece
validation hashes the assembled bytes directly when all blocks are present and
only falls back to disk verification when memory state is incomplete. These
buffers are bounded to 64 active pieces and 64 MiB per torrent task; when that
budget is exceeded, the least recently used incomplete piece buffer is evicted
and later validation falls back to the scheduled disk path. Pieces larger than
the byte budget skip in-memory assembly entirely.

Peer discovery and peer I/O pressure are also bounded. Tracker announce results
are retained in a capped cache with drop counters, metadata peer retry state is
deduplicated and pruned against the configured peer budget, and runtime stats
surface peer receive/transmit buffer bytes so the resource governor can account
for outstanding request memory alongside storage frames.

## Fastresume Contract

`clean_shutdown = true` means more than "the JSON state file was atomically
renamed." In checkpoint and strict modes it means data files were synced
according to the configured durability mode before the fastresume state was
saved. If that sync fails, the state is saved with `clean_shutdown = false`, so
startup falls back to verification instead of trusting stale piece state.

## Remaining Work

The following items are still implementation targets:

- Persist cross-device move/import execution state so interrupted multi-step
  plans can resume or be audited after process restart.
- Per-device latency observability now includes bounded Prometheus histograms
  for read/write/sync/hash work labeled by resolved device/profile, plus the
  cumulative per-device totals. Aggregate fixed-bucket histograms and counters
  also cover file-pool activity, queue depth, dirty files, sync/hash/preallocate
  work, peer-read cache, logical/backend reads, and in-memory piece assembly
  pressure.
- Replace the worker-owned fixed-buffer copy path with true frame-pool slot
  pinning once the global frame pool can lease stable registered buffer indexes.
- Extend the release hardware report with recheck runtime progress.
  `scripts/storage_hardware_matrix.sh` already records real-device seed-read
  locality, bounded hot-file descriptor reuse, elevator throughput, and
  optional syscall counts (`TNG_STORAGE_SYSCALLS=1`) across one or more target
  mounts.

## Correctness Rules

- Safe path resolution remains owned by `SafeRelPath` and `PieceMap`; peer input
  never becomes a raw filesystem path.
- Reads must never create files.
- Writes create only for known torrent file regions.
- Positioned I/O treats short reads/writes as `StorageError::ShortIo`.
- Preallocation errors fail the write path before blocks or pieces are marked
  valid.
- Fastresume must not trust valid pieces after a configured durability sync
  failure.
