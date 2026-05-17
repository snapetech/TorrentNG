# TorrentNG Scale-First Memory Burn-Down

Status as of 2026-05-17.

Run `scripts/memory_roadmap_certification.sh` to generate the current
evidence rollup and non-claim boundary report.

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
  - Applies only plans that passed admission, never overwrites destinations including broken symlinks, creates parents, verifies file or directory copy lengths, rejects symlinks in recursive copy and hardlink-import sources, uses symlink-safe delete, removes copy-move sources only after the verified rename, supports approved recursive delete, and rolls back staged copy files/directories on failure.
  - Root-confined execution validates source, destination, and rollback paths against configured storage roots before applying filesystem changes.
  - `scripts/storage_move_import_certification.sh` records the local planner/executor and full storage suite evidence under `certification/reports/`.
  - Set `TNG_STORAGE_MOVE_IMPORT_ROOT` to run the same executor on a real storage root with configurable fixture size.

- [x] Current HDD/NVMe real-device release evidence with required HDD 5x target.
  - Local NVMe/SSD report: `certification/reports/storage-hardware-20260517T201259Z.md`.
  - HDD-backed LVM media pool report: `certification/reports/storage-hardware-kspls0-lvm-hdd-20260517T201732Z.md`.
  - The HDD report proves behavior on `/mnt/datapool_lvm_media` as the OS exposes it: one ext4 LV on `/dev/dm-0` over rotational LVM PVs. It does not prove per-physical-drive placement control.

- [x] Sampled per-physical-drive placement evidence inside the LVM media pool.
  - LVM/PV map report: `certification/reports/storage-hardware-kspls0-lvm-pvmap-20260517T202609Z.md`.
  - The sampled independent files mapped to multiple rotational PVs (`/dev/sdb` and `/dev/sdh`) while TorrentNG still sees one logical device (`dm-0`).
  - This proves the pool can allocate separate files on different physical disks. It does not give path-level control over which PV receives a file.

## Optional Acceleration

- [ ] `io_uring` graduation behind the backend interface.
  - Correctness fallback and forced-roundtrip tests exist.
  - Real-device hardware reports now include `pread` and forced `uring`
    backend roundtrips plus registered-file/fixed-buffer capability rows.
  - `scripts/storage_uring_graduation.sh` records real-device `pread` vs
    `uring` stream throughput, selected backend, registered-file support, and
    fixed-buffer support, with opt-in pass/fail thresholds for graduation.
  - `uring` remains explicit opt-in until real-device `pread` vs `uring` reports show a durable win and frame-pool registered-buffer leases remove the extra copy path.
