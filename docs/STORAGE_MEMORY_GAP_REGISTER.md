# Storage And Memory Gap Register

This register is the implementation-facing status for the current storage and
memory hardening work on `main`. It exists because the burndown and phase docs
describe the intended architecture, while this file tracks the remaining gaps
that still need engineering evidence or follow-up code.

The storage-only implementation map lives in
[`STORAGE_WORK_MAP.md`](STORAGE_WORK_MAP.md). Treat that file as the current
source of truth for which storage path is live and which work remains.

## Current Status

Implemented and covered by automated tests:

- Positioned storage I/O through `MountScheduler` with bounded dedicated disk
  workers instead of Tokio's shared blocking pool.
- Path-keyed open-file cache with hit/miss/eviction/idle-close counters and
  Unix fd-limit clamping.
- Per-file preparation in torrent tasks so parent creation and allocation are
  outside the per-block hot path.
- `PreallocationMode::Auto` topology policy: full allocation only for local
  rotational non-CoW storage, sparse otherwise.
- Checkpoint and strict durability modes with fastresume `clean_shutdown`
  gated by storage sync success.
- Separate hashing pool and RAM-first completed-piece verification.
- Peer-read readahead cache and HDD peer-read elevator with coalescing metrics.
- Sparse recheck via hole/data probing with fallback counters.
- Resource-governor classes for storage frames, piece assembly, peer buffers,
  webseed bodies, metadata, tracker peers, DHT table, API snapshots, and queued
  disk work.
- Native metrics for hot-torrent memory attribution and queued disk bytes.
- Dirty-path tracking survives open-file cache eviction: checkpoint sync
  reopens and syncs dirty files that are no longer cached.
- Per-device storage latency has bounded Prometheus histograms for
  read/write/sync/hash work, labeled by resolved device and profile.
- Queued disk/hash/elevator work reserves actual queued payload bytes before
  enqueue and releases them on rejection, cancellation, or completion.
- File-pool metadata memory is attributed through scheduler, engine, and native
  Prometheus stats.
- Schedulers that resolve to the same storage device share a process-level
  device queue semaphore for all positioned disk submissions.
- Move/import/delete plans have a conservative executor with no-overwrite
  admission, parent creation, copy-length verification, staged rollback cleanup,
  recursive directory copy/delete support, copy-based move source cleanup after
  verified rename, symlink rejection for rename/copy/hardlink-import sources,
  symlink-safe no-overwrite checks, symlink-safe delete, and dry-run no-op
  behavior.
- Move/import/delete execution has an opt-in storage-root confinement entry
  point that validates source, destination, and rollback paths before applying
  any filesystem change.
- Move/import/delete certification can run against a real storage root with
  configurable fixture size by setting `TNG_STORAGE_MOVE_IMPORT_ROOT`,
  `TNG_STORAGE_MOVE_IMPORT_FILES`, and `TNG_STORAGE_MOVE_IMPORT_MIB_PER_FILE`.
- Real-device storage reports include explicit `pread` and forced `uring`
  backend roundtrips with selected backend, fallback reason, registered-file
  support, fixed-buffer support, batch length, and fixed-buffer length.
- Forced `io_uring` selection falls back to `pread` with a diagnostic if worker
  startup cannot create the ring on the host/container.
- `PreadBackend` and `UringBackend` have bounded internal queues and fail
  closed with `WouldBlock` when the backend queue is full.
- The live torrent scheduler owns a `SelectedDiskBackend` and routes reads,
  writes, syncs, and peer-read elevator dispatch through the same bounded
  backend interface used by storage runtime probes.
- Scheduler read and peer-read elevator buffers enter the backend through frame
  objects before being copied into caller-owned `Bytes`; process-level
  frame-pool accounting for the live torrent path remains tracked below.
- Native `[storage]` TOML covers scheduler `StorageIoConfig` knobs for file
  pool size, idle TTL, I/O/hash workers, queue depths, preallocation,
  durability, peer-read readahead/cache, and elevator budget.
- `scripts/storage_uring_graduation.sh` records real-device `pread` vs
  `uring` stream throughput and optional graduation thresholds.
- Current real-device storage evidence includes local NVMe/SSD and the kspls0
  HDD-backed LVM media pool. The LVM report passes the required 5x wall-clock
  target at 8192 blocks and collapses backend reads from 8192 to 1.
- The kspls0 LVM/PV extent probe sampled independent files mapping to multiple
  rotational PVs (`/dev/sdb` and `/dev/sdh`) under the same logical LV.

## Remaining Gaps

| Area | Gap | Risk | Next Work |
| --- | --- | --- | --- |
| Storage frame accounting on torrent reads | Scheduler reads now dispatch through `DiskBackend`, but still copy backend frames into `Bytes`; the global `FramePool` only backs `StorageRuntime::read_frame`. | Storage frame caps do not bound the primary torrent read/recheck path, so frame metrics can understate actual storage buffer pressure. | Add scheduler frame leases or make scheduler reads consume `StorageRuntime` frames before dispatching disk work. |
| Scheduler/backend double queue | Scheduler direct reads, writes, syncs, and peer-read elevator dispatch now await backend completion outside the scheduler blocking pool after a short open/metadata phase. Prepare and sparse extent probing still use the older bounded blocking bridge. | The hottest payload, durability, and elevator paths are improved, but secondary paths can still occupy scheduler workers while waiting on backend completion. | Apply the same guard/open/backend split to remaining backend I/O paths, then remove blocking backend waits from scheduler code. |
| Restartable move/import execution | The move/import/delete executor is conservative for one process execution, but multi-step move/import progress is not durably recorded. | A process crash during a multi-TB move leaves recovery dependent on filesystem inspection instead of a known persisted plan state. | Persist plan id, steps, completed steps, and rollback intent in the engine DB before executing cross-device copy/rename/delete operations. |
| Deterministic LVM PV placement control | The kspls0 extent probe shows the pool can allocate independent files on multiple rotational PVs, but ordinary path writes still do not let TorrentNG choose a specific PV. | Cross-PV behavior inside the LVM pool is allocator-dependent, so path-level scheduling cannot promise physical-drive affinity. | Use LVM extent mapping for evidence, or add lower-level PV-targeted probes only if release claims require deterministic per-drive placement. |
| `io_uring` frame-pool slot pinning | `UringBackend` uses worker-owned fixed buffers when available, but the global frame pool does not yet hand out stable registered buffer slots. | Extra copies remain in the uring path, and fixed-buffer metrics can overstate how much of the full storage path is zero-copy. | Run `scripts/storage_uring_graduation.sh /target/root` with selected-backend, fixed-buffer, registered-file, and throughput thresholds. Add frame-pool slot leases through the backend API only after those reports prove `uring` should graduate from explicit opt-in. |
| Move/import certification | The certification runner now supports real-root fixture execution, but representative multi-TB operator evidence is still host/run dependent. | Large library move/import claims should not be made from unit tests alone. | Run `TNG_STORAGE_MOVE_IMPORT_ROOT=/target/root TNG_STORAGE_MOVE_IMPORT_FILES=... TNG_STORAGE_MOVE_IMPORT_MIB_PER_FILE=... scripts/storage_move_import_certification.sh` on the target storage roots and publish the generated report. |

## Verification Commands

Use these before claiming the current branch is healthy:

```sh
cargo fmt --check
cargo test -p rt-storage
cargo test -p rt-engine
cargo test -p rt-metrics resource::tests
cargo test -p rt-api-native render_metrics_includes_engine_stats
scripts/storage_ng_feature_matrix.sh
scripts/api_facade_certification.sh
scripts/storage_move_import_certification.sh
TNG_STORAGE_MOVE_IMPORT_ROOT=/target/root scripts/storage_move_import_certification.sh
scripts/storage_uring_graduation.sh /target/root
scripts/memory_roadmap_certification.sh
```

Use these before claiming production storage performance:

```sh
TNG_STORAGE_REQUIRE_HDD_5X=1 scripts/storage_hardware_matrix.sh /mnt/nvme /mnt/hdd
```
