# Storage Work Map

This is the storage-only implementation map for `main`. It is intentionally
code-facing: each row identifies the live path, the evidence already in-tree,
and the remaining work needed before claiming the feature complete.

## Live Data Paths

| Path | Owner | Current use | Notes |
| --- | --- | --- | --- |
| Torrent payload I/O | `rt-storage::MountScheduler` | Download writes, seed reads, recheck reads, v2 file verification, fastresume syncs | This is the production torrent hot path. It owns per-class semaphores, its own file pool, a scheduler-owned `DiskBackend`, hashing pool, peer-read cache, and HDD peer-read elevator. |
| Global storage runtime | `rt-storage::StorageRuntime` | Backend capability metrics, frame-pool metrics, direct backend tests and real-device backend probes | This path owns the process-level `FramePool` and `HandleCache` API. It uses the same `DiskBackend` implementations and global frame pool as `MountScheduler`. |
| Move/import/delete executor | `rt-storage::plan` | Planned library movement and deletion helpers | Covered by unit tests and an optional real-root certification script. It is separate from per-block torrent I/O. |

## Implemented In The Torrent Hot Path

| Area | Status | Evidence |
| --- | --- | --- |
| Positioned reads/writes | Implemented through a scheduler-owned `SelectedDiskBackend` | `crates/rt-storage/src/scheduler.rs` `read_at`, `write_at`; `crates/rt-storage/src/backend.rs`; `concurrent_positioned_writes_do_not_share_cursor` |
| Dedicated blocking workers | Implemented in `MountScheduler::submit` through its bounded `BlockingPool` | `crates/rt-storage/src/scheduler.rs`; `full_mount_queue_fails_closed`, `queued_disk_bytes_track_active_blocking_job_payload` |
| Open-file pool | Implemented for scheduler hot path | `FilePoolStats`; `file_pool_records_hits_and_evictions`; real-device `repeated_reads_reuse_one_open_file_handle` |
| Per-file preparation | Implemented in `TorrentTask` with a prepared-file registry | `crates/rt-engine/src/torrent_task.rs`; `sparse_prepare_creates_parent_once` |
| Auto preallocation | Implemented from detected topology | `preallocation_mode_for_topology`; `auto_preallocation_policy_uses_full_only_for_non_cow_hdd` |
| Durability barrier | Implemented for scheduler dirty files and fastresume trust | `sync_data`, `sync_all_open_files`; fastresume save paths in `TorrentTask` |
| Hash isolation | Implemented with a dedicated scheduler hash pool | `hash_sha1`, `hash_v2_leaf`, `hash_v2_root`; scale tests for hash-pool isolation |
| Peer-read locality | Implemented as bounded readahead cache and HDD elevator | `peer_read_readahead_cache_*`, `hdd_peer_read_elevator_*`, real-device benchmark probes |
| Sparse recheck | Implemented via data/hole probing where supported | `data_extents`; `verify_sparse_piece_hashes_holes_as_zeroes`; `sparse_recheck_skips_holes_and_reports_extent_counters` |
| Backend queue bounds | Implemented for `PreadBackend` and `UringBackend` with bounded sync queues and immediate `WouldBlock` failures on full queue | `pread_backend_queue_fails_closed_when_full`; `backend::tests` |
| Hot-path backend integration | Implemented for scheduler reads, writes, syncs, and peer-read elevator dispatch | `MountScheduler::disk_backend`; `SelectedDiskBackend::select_with_queue_depth`; `read_and_write_roundtrip`; `hdd_peer_read_elevator_*` |
| Async backend waits | Implemented for live read/write/sync and peer-read elevator backend operations | Backend completion is awaited outside scheduler blocking workers after short open/metadata phases; remaining `submit` users are prepare and sparse extent metadata work |
| Shared storage frame pool | Implemented for `StorageRuntime`, direct scheduler reads, and peer-read elevator reads | `global_frame_pool`; `StorageRuntime::read_frame`; `MountScheduler::read_at`; `dispatch_peer_read_batch`; `frame::tests` |
| Frame-owned read API | Implemented beside the `Bytes` compatibility path and adopted for upload assembly reads | `MountScheduler::read_owned_at`; `scheduled_read_owned`; exact backend reads return `StorageRead::Frame`; `read_upload_block` consumes `StorageRead::as_slice` |
| Returned read accounting | Implemented for live upload blocks and scheduler-owned peer-read cache entries | Upload blocks hold `PeerBuffer` leases through send; peer-read cache entries hold `PeerBuffer` leases while cached |
| Stable `io_uring` file slots | Implemented as a conservative per-worker file-identity table | `UringWorker::file_slots`; file slots are keyed by device/inode and fall back to raw fd when the table is full |
| Restartable move/import primitive | Implemented at the storage-plan executor and engine job boundaries | `execute_storage_plan_with_checkpoints`; `execute_storage_plan_under_roots_with_checkpoints`; checkpointed steps are skipped on resume; engine storage-plan jobs persist queued/running/checkpoint/completed state |
| Save-path move execution | Implemented for engine field updates | `update_torrent_fields_inner`; `move_torrent_payload_files`; qBit/native set-location paths now move existing payload files before committing `save_path` |
| Native storage plan API | Implemented for preview and execution | `/api/v1/storage/plan`; `/api/v1/storage/execute`; execution goes through durable engine storage-plan jobs |
| Storage plan UI workflow | Implemented in the Library storage panel | Operators can select move/import/delete, choose a writable storage root, preview root-confined steps and issues, then execute through the native durable storage-plan API |
| Native storage config | Implemented for scheduler `StorageIoConfig` knobs in `[storage]` TOML | `rt-config` storage defaults/parse tests; `storage_io_config_maps_native_storage_toml` |
| Root-confined move/import/delete | Implemented as a separate planned executor | `execute_storage_plan_under_roots`; move/import certification script |

## Remaining Storage Work

| Priority | Area | What remains | Why it matters | First concrete task |
| --- | --- | --- | --- | --- |
| P2 | Zero-copy returned-read adoption | Storage now exposes a frame-owned read API while keeping `scheduled_read -> Bytes` compatibility. Upload block assembly and sparse recheck extent reads use the owned path; compatibility conversion consumes frames into `Bytes` without copying payload bytes, and `scripts/storage_ng_feature_matrix.sh` fails if new production callers add direct `scheduled_read` use. | Avoids broad API churn while removing the compatibility copy from measured read returns. | Profile release workloads and migrate any new measured hot call sites from `scheduled_read` to `scheduled_read_owned` if retaining frame-pool reuse matters more than `Bytes` ownership. |
| P1 | `io_uring` fixed-buffer/frame integration | `UringBackend` reports its fixed-buffer strategy explicitly: current registered fixed buffers are `worker_copy`, not `frame_pool_slots`, and the graduation script can enforce `TNG_STORAGE_URING_REQUIRE_FRAME_POOL_SLOTS=1`. | Operators can no longer confuse fixed-buffer kernel support with application-frame zero-copy, but the extra copy remains until frame-pool slot leases exist. | Add frame-pool slot leases with stable registered buffer indexes, then benchmark `pread` vs `uring` on target hardware before changing `auto`. |
| P2 | Move/import product workflow breadth | The Library storage panel previews and executes move/import/delete plans, includes affected-torrent metadata, accepts completed-step indexes for resumed durable jobs, offers move/import/delete/resume templates, summarizes forward/rollback step counts and bytes, and polls active storage-plan jobs for step/byte progress. | Large operator workflows may still need more guardrails than a single-plan form. | Extend from real operator runs once larger batch needs are concrete. |
| P2 | Hardware evidence automation | Storage hardware, `io_uring`, and move/import certification scripts now update a generated certification report index. The hardware matrix includes seed-read locality, hot-fd reuse, recheck runtime progress, elevator throughput, optional syscall counts, and optional LVM/PV extent evidence. Operator target roots and hardware runs are still host supplied. | Production claims still need actual HDD/NVMe/network filesystem evidence, not unit tests alone. | Run the certification scripts on target hardware and publish the generated `certification/reports/storage-certification-index.md` with the release artifacts. |
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
