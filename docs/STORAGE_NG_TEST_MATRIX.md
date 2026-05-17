# Storage NG Feature Test Matrix

This matrix covers the current Storage NG feature set on `main`: bounded file
handles, frames, peer caches, and peer buffers; positioned I/O;
topology-derived scheduling; peer-read locality; durability barriers; sparse
recheck; RAM-first completed-piece verification; runtime backend selection;
and resource-governor observability.

Run the local automated matrix with:

```sh
scripts/storage_ng_feature_matrix.sh
```

Set `STORAGE_NG_REAL_DEVICE=1` and `TNG_STORAGE_BENCH_DIR=/mnt/target` to add
the ignored real-device probes.

For release hardware evidence across multiple mounts, run:

```sh
scripts/storage_hardware_matrix.sh /mnt/nvme /mnt/hdd
```

The hardware matrix writes a markdown report under `certification/reports/`,
prints topology for each target, records `pread` and forced `uring` backend
selection/capability roundtrips, and enforces the HDD elevator speedup gate when
`TNG_STORAGE_REQUIRE_HDD_5X=1` is set. Set `TNG_STORAGE_SYSCALLS=1` to include
per-case syscall counters when `strace` is available.

## Automated Matrix

| Area | Coverage | Command |
| --- | --- | --- |
| Formatting | Workspace Rust formatting | `cargo fmt --check` |
| Storage core | fd pool, positioned I/O, preallocation, durability, page-cache advice, sparse recheck, readahead, topology, elevator policy | `cargo test -p rt-storage` |
| Backend selection | `auto`/`pread`/`uring` parsing, `io_uring` probe and worker-start fallback diagnostics, selected-backend read/write roundtrip | `cargo test -p rt-storage backend::tests` |
| Backend graduation | real-device `pread` vs `uring` stream throughput, selected backend, registered-file support, fixed-buffer support | `scripts/storage_uring_graduation.sh /target/root` |
| Resource governor | total and per-class memory caps, pressure transitions, denied allocation counters, lease release | `cargo test -p rt-metrics resource::tests` |
| Scale proxies | bounded crash recheck, storage fd cap, peer-read locality, hash-pool isolation, RAM verify path, sparse recheck extents | `cargo test -p rt-metrics storage_ -- --nocapture` |
| Configuration | default storage elevator, memory caps, runtime tier switch, TOML partial parsing | `cargo test -p rt-config` |
| Engine consumers | storage-backed recheck, upload reads across multi-file regions, resource snapshot in engine stats, taskless v2 verification | `cargo test -p rt-engine` |
| Native API metrics | Prometheus projection for storage backend, frame/fd runtime, scheduler counters, bounded peer-cache pressure, peer buffer bytes, resource-governor classes | `cargo test -p rt-api-native render_metrics_includes_engine_stats` |
| Move/import executor | plan admission, symlink-safe no-overwrite moves, copy-based move source cleanup, hardlink-or-copy import, recursive directory copy/delete, rename/copy/import symlink rejection, symlink-safe delete, staged rollback cleanup, storage-root confinement, optional real-root fixture execution | `scripts/storage_move_import_certification.sh`; set `TNG_STORAGE_MOVE_IMPORT_ROOT` for hardware-root evidence |

## Runtime Configuration Matrix

| Setting | Values | Expected behavior |
| --- | --- | --- |
| `TNG_STORAGE_BACKEND` | `auto`, `pread`, `uring` | `auto` uses the dedicated positioned-I/O worker-pool baseline; `pread` explicitly requests that baseline; `uring` requests the Linux `io_uring` backend and falls back with a diagnostic when unavailable |
| `TNG_STORAGE_DISK_THREADS` | positive integer | Sets dedicated backend worker count for the selected/fallback backend |
| `TNG_STORAGE_FRAME_CAP_MB` | positive integer | Caps global storage frame memory; exhausted frames return queue/backpressure errors rather than unbounded allocation |
| `TNG_STORAGE_HANDLE_IDLE_SECS` | positive integer | Controls idle cached-handle close latency |
| `TNG_STORAGE_SYSCALLS` | `0`, `1` | Adds `strace` syscall counts to real-device hardware matrix summaries when available |
| `[memory].*` | TOML config | Sets resource-governor total and class caps exposed through `/metrics` |

## Prometheus Checks

| Metric | Expected proof |
| --- | --- |
| `torrentng_storage_backend_selected{backend=...}` | Exactly one active backend sample with value `1` |
| `torrentng_storage_backend_fixed_buffers_supported` | `0` for `pread`; `1` for `uring` when the kernel accepts registered worker buffers |
| `torrentng_storage_backend_registered_files_supported` | `0` for `pread`; `1` for `uring` when the kernel accepts registered file slots |
| `torrentng_storage_backend_{max_batch_len,fixed_buffer_bytes}` | Backend batch and fixed-buffer sizing match the selected implementation |
| `torrentng_storage_*_latency_nanoseconds_by_device{device=...,profile=...}` | Storage latency attribution and bounded histograms survive multi-device aggregation |
| `torrentng_storage_file_pool_*` | fd pool remains bounded and records hit/miss/eviction/idle-close activity |
| `torrentng_storage_queue_full_total` | bounded storage queues expose backpressure instead of silently accumulating jobs |
| `torrentng_storage_peer_read_cache_*`, `torrentng_storage_peer_read_elevator_*` | Bounded peer-read readahead cache hit/miss/eviction behavior and HDD peer-read queueing, batching, and coalescing are visible |
| `torrentng_storage_sparse_*` | recheck reports sparse data extents, skipped holes, and fallback count |
| `torrentng_tracker_peer_cache_entries`, `torrentng_tracker_peer_cache_drops_total` | Tracker announce responses stay bounded and expose dropped overflow peers |
| `torrentng_peer_{rx,tx}_buffer_bytes`, `torrentng_peer_command_queue_*`, `torrentng_memory_class_used_bytes{class="peer_buffer"}` | Outstanding peer request/send buffers, upload block leases, and bounded peer command queues are visible for memory-pressure accounting |
| `torrentng_memory_*` | process-owned memory cap, pressure state, per-class usage, and denied allocations are visible |

## Real-Device Matrix

Run:

```sh
STORAGE_NG_REAL_DEVICE=1 TNG_STORAGE_BENCH_DIR=/mnt/target scripts/storage_ng_feature_matrix.sh
```

For a mixed-storage host, prefer:

```sh
TNG_STORAGE_REQUIRE_HDD_5X=1 scripts/storage_hardware_matrix.sh /mnt/nvme /mnt/hdd
```

| Platform | Required evidence |
| --- | --- |
| HDD ext4/xfs | rotational topology, full preallocation under `Auto`, peer-read elevator backend-read reduction, bounded fd reuse |
| HDD btrfs/zfs/bcachefs | rotational topology with CoW detection, sparse preallocation under `Auto`, no full-allocation fallback spam |
| SATA SSD | SSD profile, sparse preallocation, low/zero elevator budget |
| NVMe | NVMe profile by parent block device name, sparse preallocation, backend selection remains independent of topology |
| NFS/CIFS/virtiofs | network `DeviceId` from mount source, sparse preallocation, reads never create missing files |
| Container overlay | CoW/unknown-safe sparse allocation and clean `io_uring` fallback diagnostics when kernel policy disables it |
| Backend comparison | `tng_storage_backend requested=pread` and `requested=uring` rows report selected backend, fallback reason, registered-file support, fixed-buffer support, batch length, and fixed-buffer length |

## Exit Criteria

- `scripts/storage_ng_feature_matrix.sh` passes locally.
- Real-device probes pass on at least one HDD and one SSD/NVMe host before
  claiming production storage performance.
- `clean_shutdown = true` is only trusted when the configured durability mode
  completed its storage sync requirement.
- `UringBackend` remains a tuning target until worker-owned fixed buffers are
  replaced by true frame-pool slot pinning and benchmarked on real hardware.
