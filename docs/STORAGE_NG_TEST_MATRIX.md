# Storage NG Feature Test Matrix

This matrix covers the current Storage NG feature set on `main`: bounded file
handles and frames, positioned I/O, topology-derived scheduling, peer-read
locality, durability barriers, sparse recheck, RAM-first completed-piece
verification, runtime backend selection, and resource-governor observability.

Run the local automated matrix with:

```sh
scripts/storage_ng_feature_matrix.sh
```

Set `STORAGE_NG_REAL_DEVICE=1` and `TNG_STORAGE_BENCH_DIR=/mnt/target` to add
the ignored real-device probes.

## Automated Matrix

| Area | Coverage | Command |
| --- | --- | --- |
| Formatting | Workspace Rust formatting | `cargo fmt --check` |
| Storage core | fd pool, positioned I/O, preallocation, durability, page-cache advice, sparse recheck, readahead, topology, elevator policy | `cargo test -p rt-storage` |
| Backend selection | `auto`/`pread`/`uring` parsing, `io_uring` probe fallback diagnostics, selected-backend read/write roundtrip | `cargo test -p rt-storage backend::tests` |
| Resource governor | total and per-class memory caps, pressure transitions, denied allocation counters, lease release | `cargo test -p rt-metrics resource::tests` |
| Scale proxies | bounded crash recheck, storage fd cap, peer-read locality, hash-pool isolation, RAM verify path, sparse recheck extents | `cargo test -p rt-metrics storage_ -- --nocapture` |
| Configuration | default storage elevator, memory caps, runtime tier switch, TOML partial parsing | `cargo test -p rt-config` |
| Engine consumers | storage-backed recheck, upload reads across multi-file regions, resource snapshot in engine stats, taskless v2 verification | `cargo test -p rt-engine` |
| Native API metrics | Prometheus projection for storage backend, frame/fd runtime, scheduler counters, resource-governor classes | `cargo test -p rt-api-native render_metrics_includes_engine_stats` |

## Runtime Configuration Matrix

| Setting | Values | Expected behavior |
| --- | --- | --- |
| `TNG_STORAGE_BACKEND` | `auto`, `pread`, `uring` | `auto` probes the best available backend; `pread` forces the dedicated positioned-I/O worker pool; `uring` requests the Linux `io_uring` backend and falls back with a diagnostic when unavailable |
| `TNG_STORAGE_DISK_THREADS` | positive integer | Sets dedicated backend worker count for the selected/fallback backend |
| `TNG_STORAGE_FRAME_CAP_MB` | positive integer | Caps global storage frame memory; exhausted frames return queue/backpressure errors rather than unbounded allocation |
| `TNG_STORAGE_HANDLE_IDLE_SECS` | positive integer | Controls idle cached-handle close latency |
| `[memory].*` | TOML config | Sets resource-governor total and class caps exposed through `/metrics` |

## Prometheus Checks

| Metric | Expected proof |
| --- | --- |
| `torrentng_storage_backend_selected{backend=...}` | Exactly one active backend sample with value `1` |
| `torrentng_storage_backend_fixed_buffers_supported` | `0` for `pread`; `1` for `uring` when the kernel accepts registered worker buffers |
| `torrentng_storage_*_latency_nanoseconds_by_device_total{device=...,profile=...}` | Storage latency attribution survives multi-device aggregation |
| `torrentng_storage_file_pool_*` | fd pool remains bounded and records hit/miss/eviction/idle-close activity |
| `torrentng_storage_peer_read_elevator_*` | HDD peer-read queueing, batching, and coalescing are visible |
| `torrentng_storage_sparse_*` | recheck reports sparse data extents, skipped holes, and fallback count |
| `torrentng_memory_*` | process-owned memory cap, pressure state, per-class usage, and denied allocations are visible |

## Real-Device Matrix

Run:

```sh
STORAGE_NG_REAL_DEVICE=1 TNG_STORAGE_BENCH_DIR=/mnt/target scripts/storage_ng_feature_matrix.sh
```

| Platform | Required evidence |
| --- | --- |
| HDD ext4/xfs | rotational topology, full preallocation under `Auto`, peer-read elevator backend-read reduction, bounded fd reuse |
| HDD btrfs/zfs/bcachefs | rotational topology with CoW detection, sparse preallocation under `Auto`, no full-allocation fallback spam |
| SATA SSD | SSD profile, sparse preallocation, low/zero elevator budget |
| NVMe | NVMe profile by parent block device name, sparse preallocation, backend selection remains independent of topology |
| NFS/CIFS/virtiofs | network `DeviceId` from mount source, sparse preallocation, reads never create missing files |
| Container overlay | CoW/unknown-safe sparse allocation and clean `io_uring` fallback diagnostics when kernel policy disables it |

## Exit Criteria

- `scripts/storage_ng_feature_matrix.sh` passes locally.
- Real-device probes pass on at least one HDD and one SSD/NVMe host before
  claiming production storage performance.
- `clean_shutdown = true` is only trusted when the configured durability mode
  completed its storage sync requirement.
- `UringBackend` remains a tuning target until worker-owned fixed buffers are
  replaced by true frame-pool slot pinning and benchmarked on real hardware.
