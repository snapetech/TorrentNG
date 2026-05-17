# TorrentNG Scale-First Memory Burn-Down

Status as of 2026-05-17.

## Completed

- [x] Hard `QueuedDisk` memory leases before disk/hash/elevator enqueue.
  - `MountScheduler` now reserves `MemoryClass::QueuedDisk` before per-mount disk jobs, hash jobs, and peer-read elevator enqueue.
  - Queue admission fails closed with `StorageError::QueueFull` when the queued-disk class cap is exhausted.
- [x] Hot-torrent memory attribution no longer relies on coarse fixed constants where local structures can report capacity.
  - Tracker peer cache attribution uses `HashSet` capacity and `SocketAddr` size.
  - Peer command queue attribution uses channel capacity and `PeerCommand` size.
  - Storage cache attribution uses file-pool entry/path memory reported by the scheduler.
- [x] Process-level per-device scheduler registry for positioned disk submissions.
  - Schedulers resolving to the same device/profile share a global device queue semaphore.
  - The shared device queue applies to read/write/sync/recheck/preallocation submissions and peer-read elevator dispatch.
- [x] 10k/100k idle torrent RSS/task/fd evidence proxy.
  - `idle_memory_100k_keeps_fixed_rss_task_fd_budget` checks 100k idle API shape under fixed RSS, task, and fd growth targets.
- [x] 1k hot seeding memory-cap evidence proxy.
  - `hot_seeding_1k_memory_attribution_stays_under_cap` checks 1k synthetic hot seeders against the current top-hot attribution cap.
- [x] Slow-disk plus fast-peer backpressure evidence proxy.
  - Storage scale tests exercise queue-full backpressure rather than unbounded queue growth under saturated hash/disk paths.
- [x] Conservative move/import/delete executor below the storage planner.
  - Applies only plans that passed admission, never overwrites destinations, creates parents, verifies copy lengths, and rolls back staged copy files on failure.

## Hardware-Gated

- [ ] Current HDD/NVMe real-device release evidence with required HDD 5x target.
  - Run `TNG_STORAGE_REQUIRE_HDD_5X=1 scripts/storage_hardware_matrix.sh /mnt/nvme /mnt/hdd` on representative devices.
  - Keep the generated report under `certification/reports/`.

## Optional Acceleration

- [ ] `io_uring` graduation behind the backend interface.
  - Correctness fallback and forced-roundtrip tests exist.
  - `uring` remains explicit opt-in until real-device `pread` vs `uring` reports show a durable win and frame-pool registered-buffer leases remove the extra copy path.
