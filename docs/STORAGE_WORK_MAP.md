# Storage Work Map

This is the storage-only implementation map for `main`. It is intentionally
code-facing: each row identifies the live path, the evidence already in-tree,
and the remaining work needed before claiming the feature complete.

## Live Data Paths

| Path | Owner | Current use | Notes |
| --- | --- | --- | --- |
| Torrent payload I/O | `rt-storage::MountScheduler` | Download writes, seed reads, recheck reads, v2 file verification, fastresume syncs | This is the production torrent hot path. It owns per-class semaphores, its own file pool, its own blocking I/O pool, hashing pool, peer-read cache, and HDD peer-read elevator. |
| Global storage runtime | `rt-storage::StorageRuntime` | Backend capability metrics, frame-pool metrics, direct backend tests and real-device backend probes | This path owns `DiskBackend`, `PreadBackend`, `UringBackend`, `FramePool`, and `HandleCache`. It is not yet the backend used by `MountScheduler` for torrent payload reads/writes. |
| Move/import/delete executor | `rt-storage::plan` | Planned library movement and deletion helpers | Covered by unit tests and an optional real-root certification script. It is separate from per-block torrent I/O. |

## Implemented In The Torrent Hot Path

| Area | Status | Evidence |
| --- | --- | --- |
| Positioned reads/writes | Implemented with platform `FileExt` calls inside `MountScheduler` | `crates/rt-storage/src/scheduler.rs` `read_at`, `write_at`, `positioned_read`, `positioned_write`; `concurrent_positioned_writes_do_not_share_cursor` |
| Dedicated blocking workers | Implemented in `MountScheduler::submit` through its bounded `BlockingPool` | `crates/rt-storage/src/scheduler.rs`; `full_mount_queue_fails_closed`, `queued_disk_bytes_track_active_blocking_job_payload` |
| Open-file pool | Implemented for scheduler hot path | `FilePoolStats`; `file_pool_records_hits_and_evictions`; real-device `repeated_reads_reuse_one_open_file_handle` |
| Per-file preparation | Implemented in `TorrentTask` with a prepared-file registry | `crates/rt-engine/src/torrent_task.rs`; `sparse_prepare_creates_parent_once` |
| Auto preallocation | Implemented from detected topology | `preallocation_mode_for_topology`; `auto_preallocation_policy_uses_full_only_for_non_cow_hdd` |
| Durability barrier | Implemented for scheduler dirty files and fastresume trust | `sync_data`, `sync_all_open_files`; fastresume save paths in `TorrentTask` |
| Hash isolation | Implemented with a dedicated scheduler hash pool | `hash_sha1`, `hash_v2_leaf`, `hash_v2_root`; scale tests for hash-pool isolation |
| Peer-read locality | Implemented as bounded readahead cache and HDD elevator | `peer_read_readahead_cache_*`, `hdd_peer_read_elevator_*`, real-device benchmark probes |
| Sparse recheck | Implemented via data/hole probing where supported | `data_extents`; `verify_sparse_piece_hashes_holes_as_zeroes`; `sparse_recheck_skips_holes_and_reports_extent_counters` |
| Backend queue bounds | Implemented for `PreadBackend` and `UringBackend` with bounded sync queues and immediate `WouldBlock` failures on full queue | `pread_backend_queue_fails_closed_when_full`; `backend::tests` |
| Root-confined move/import/delete | Implemented as a separate planned executor | `execute_storage_plan_under_roots`; move/import certification script |

## Remaining Storage Work

| Priority | Area | What remains | Why it matters | First concrete task |
| --- | --- | --- | --- | --- |
| P0 | Unify torrent I/O with `DiskBackend` | `MountScheduler` still calls `FileExt` directly and does not submit through `SelectedDiskBackend`, so `TNG_STORAGE_BACKEND=uring` and backend batch/fixed-buffer support do not affect live torrent payload I/O. | Backend metrics can imply backend coverage that the torrent hot path does not yet use. It also leaves `io_uring` isolated to probes/runtime calls instead of the actual disk scheduler. | Introduce a scheduler-owned backend field and route `read_at`, `write_at`, and sync through the `DiskBackend` trait while preserving class permits, queue permits, dirty tracking, and stats. |
| P0 | One storage memory accounting path | `MountScheduler` allocates `Vec<u8>`/`Bytes` for reads and readahead; the global `FramePool` only backs `StorageRuntime::read_frame`. | Storage frame metrics and caps do not bound the primary torrent read path. Large peer-read batches and rechecks are controlled by queued-disk leases, but not by the frame pool. | Add frame leases to scheduler reads/elevator reads, or make the scheduler consume `StorageRuntime` frames through a backend API that can return `Bytes` without unbounded allocation. |
| P1 | `io_uring` registered-file slot ownership | `UringWorker` rotates sparse registered-file slots per submission. Slots are not a stable cache keyed by fd, and reuse is not coordinated with in-flight operations beyond immediate completion batching. | It limits the real value of registered files and needs careful safety/performance evidence before production enablement. | Add a fixed-file table keyed by raw fd/open handle identity with refcounts or only advertise registered-file support for the stable table mode. |
| P1 | `io_uring` fixed-buffer/frame integration | `UringBackend` uses worker-owned fixed buffers and copies into/out of scheduler/runtime buffers. | Fixed-buffer metrics do not yet mean the application frame pool is registered or zero-copy. | Add frame-pool slot leases with stable registered buffer indexes, then benchmark `pread` vs `uring` on target hardware before changing `auto`. |
| P1 | Config surface for storage I/O | Native config currently exposes only a small subset of scheduler knobs. Most `StorageIoConfig` values use defaults unless tests construct schedulers manually. | Operators cannot tune file pool size, worker count, queue depth, durability, preallocation, or readahead from TOML. | Extend `[storage]` TOML with `StorageIoConfig` fields, parse enum modes, and pass the full config into `TorrentTask` and v2 recheck schedulers. |
| P1 | Cross-device move/import resume | Planned copy/rename/delete is safe for one execution, but there is no durable manifest for resuming an interrupted multi-file move/import operation after process crash. | Multi-TB moves need restartable progress accounting, not just in-process rollback cleanup. | Persist plan steps and completed step ids in the engine DB before executing copy/rename/delete operations. |
| P2 | Hardware evidence automation | Real-device scripts exist, but the gap register still depends on operator-supplied target roots and reports. | Production claims need repeatable HDD/NVMe/network filesystem evidence, not unit tests alone. | Add a checked-in report index with date, host profile, command, and artifact path for each accepted storage certification run. |
| P2 | Physical PV affinity | LVM evidence can show which PVs received extents, but TorrentNG does not control ordinary write placement within an LV. | Avoids overclaiming per-spindle scheduling when the allocator owns physical placement. | Keep this as evidence-only unless the product needs explicit PV-targeted placement. |

## Verification Baseline

Before claiming the storage branch healthy:

```sh
cargo fmt --check
cargo test -p rt-storage
cargo test -p rt-engine
cargo test -p rt-metrics
cargo test -p rt-api-native
scripts/storage_ng_feature_matrix.sh
```

Before claiming production storage performance:

```sh
TNG_STORAGE_REQUIRE_HDD_5X=1 scripts/storage_hardware_matrix.sh /mnt/nvme /mnt/hdd
scripts/storage_uring_graduation.sh /mnt/target
TNG_STORAGE_MOVE_IMPORT_ROOT=/mnt/target scripts/storage_move_import_certification.sh
```
