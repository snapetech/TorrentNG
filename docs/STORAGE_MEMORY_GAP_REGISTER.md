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
- Move/import/delete execution can resume from caller-supplied completed step
  indexes and reports newly completed steps through checkpoint callbacks.
- Engine save-path updates execute existing payload moves through the
  checkpointed storage-plan executor and persist storage-plan job
  queue/running/checkpoint/completed state in SQLite before committing the new
  `save_path`.
- Native `/api/v1/storage/plan` and `/api/v1/storage/execute` expose
  move/import/delete plan preview and execution through the same durable
  storage-plan job path.
- The WebUI Library storage panel has a guided move/import/delete planner that
  previews root-confined steps and executes accepted plans through the native
  durable storage-plan API, including affected torrent metadata, completed-step
  resume indexes, operation templates, byte summaries, and active storage-plan
  job progress.
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
- Live scheduler read/write/sync and peer-read elevator backend completions
  are awaited outside scheduler blocking workers after short open/metadata
  phases.
- Scheduler read and peer-read elevator buffers enter the backend through the
  same process-level frame pool used by `StorageRuntime`, then copy into
  caller-owned `Bytes`.
- Live upload blocks hold `PeerBuffer` leases through message send, and
  scheduler-owned peer-read cache entries hold `PeerBuffer` leases while cached.
- Native `[storage]` TOML covers scheduler `StorageIoConfig` knobs for file
  pool size, idle TTL, I/O/hash workers, queue depths, preallocation,
  durability, peer-read readahead/cache, and elevator budget.
- `scripts/storage_uring_graduation.sh` records real-device `pread` vs
  `uring` stream throughput and optional graduation thresholds.
- `scripts/storage_certification_index.sh` keeps hardware, `io_uring`, and
  move/import evidence as separate indexed categories, and
  `scripts/certification_status.sh` reports each category separately before
  `scripts/post_soak_release_gate.sh` requires all three categories to pass.
- Scheduler read returns can keep frame ownership through `scheduled_read_owned`
  or consume frames into compatibility `Bytes` without copying payload bytes.
- Current real-device storage evidence includes local NVMe/SSD and the kspls0
  HDD-backed LVM media pool. The LVM report passes the required 5x wall-clock
  target at 8192 blocks and collapses backend reads from 8192 to 1.
- The kspls0 LVM/PV extent probe sampled independent files mapping to multiple
  rotational PVs (`/dev/sdb` and `/dev/sdh`) under the same logical LV.

## Remaining Gaps

| Area | Gap | Risk | Next Work |
| --- | --- | --- | --- |
| Deterministic LVM PV placement control | The kspls0 extent probe shows the pool can allocate independent files on multiple rotational PVs, but ordinary path writes still do not let TorrentNG choose a specific PV. | Cross-PV behavior inside the LVM pool is allocator-dependent, so path-level scheduling cannot promise physical-drive affinity. | Use LVM extent mapping for evidence, or add lower-level PV-targeted probes only if release claims require deterministic per-drive placement. |
| `io_uring` frame-pool slot pinning | `UringBackend` uses worker-owned fixed buffers when available and now exports `fixed_buffer_strategy=worker_copy`; the global frame pool does not yet hand out stable registered buffer slots. | Extra copies remain in the uring path, but metrics distinguish kernel fixed-buffer support from true application-frame zero-copy, and roadmap certification no longer treats worker-copy reports as final graduation. | Run `scripts/storage_uring_graduation.sh /target/root` with selected-backend, fixed-buffer, registered-file, throughput thresholds, and `TNG_STORAGE_URING_REQUIRE_FRAME_POOL_SLOTS=1` for the final gate. Add frame-pool slot leases through the backend API only after those reports prove `uring` should graduate from explicit opt-in. |
| Move/import certification | The certification runner now supports real-root fixture execution and indexed PASS/FAIL evidence, but representative multi-TB operator evidence is still host/run dependent. | Large library move/import claims should not be made from unit tests alone. | Run `TNG_STORAGE_MOVE_IMPORT_ROOT=/target/root TNG_STORAGE_MOVE_IMPORT_FILES=... TNG_STORAGE_MOVE_IMPORT_MIB_PER_FILE=... scripts/storage_move_import_certification.sh` on the target storage roots and publish the generated report. |

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
