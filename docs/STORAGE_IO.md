# Storage I/O

This document tracks the native engine storage path for large seedboxes. The
target is explicit userspace I/O control for 10k-100k torrents and 200+ TB
libraries without mmap as the primary data path.

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
filesystems, while full preallocation is operator-selected.

## Current Implementation

`rt-storage::MountScheduler` now owns the disk layer beneath the class
semaphores:

- `StorageIoConfig` carries file-pool size, idle TTL, I/O worker count, queue
  depth, preallocation mode, durability mode, and peer-read readahead target.
- `scheduled_read` and `scheduled_write` remain compatibility wrappers, but
  call positioned `read_at`/`write_at`.
- Disk syscalls run on a bounded dedicated worker pool instead of Tokio's shared
  blocking pool.
- SHA-1 and BEP52 leaf/root hashing run on a separate bounded hashing pool.
- The open-file pool is keyed by normalized absolute path, tracks read/write
  mode, hits, misses, evictions, idle closes, and open count.
- File-pool capacity is clamped to a conservative fraction of `RLIMIT_NOFILE`
  on Unix when the soft limit is available.
- Reads open read-only with `create(false)` and never create or truncate files.
- Writes use positioned I/O and validate short writes.
- `prepare_file` creates parents and applies `PreallocationMode::{Off, Sparse,
  Full}` before first write.
- `sync_data` and `sync_all_open_files` provide durability checkpoints.
- `DurabilityMode::Strict` syncs after writes; `Checkpoint` syncs open torrent
  files before clean fastresume saves; `Fast` preserves older relaxed behavior.
- `StorageIoStats` exposes file-pool counters, queue depths, dirty file count,
  bytes and operations by `IoClass`, sync count, hash count, and preallocation
  fallback/failure counters.

`TorrentTask` keeps a per-file preparation registry so parent directories and
file allocation are no longer in the per-block hot path.

## Fastresume Contract

`clean_shutdown = true` means more than "the JSON state file was atomically
renamed." In checkpoint and strict modes it means data files were synced
according to the configured durability mode before the fastresume state was
saved. If that sync fails, the state is saved with `clean_shutdown = false`, so
startup falls back to verification instead of trusting stale piece state.

## Remaining Work

The following items are still implementation targets:

- Export `StorageIoStats` through `rt-metrics` and Prometheus, including
  latency histograms for read/write/sync/hash work.
- Verify completed pieces from assembled in-memory download data before falling
  back to disk re-read.
- Add peer-read readahead/coalescing for adjacent requests while returning exact
  requested bytes to peer-wire callers.
- Add benchmarks comparing syscall count, seed-read locality, recheck runtime
  progress, and bounded descriptor use under active file counts above pool
  capacity.

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
