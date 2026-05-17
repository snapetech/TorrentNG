# Storage And Memory Gap Register

This register is the implementation-facing status for the current storage and
memory hardening work on `main`. It exists because the burndown and phase docs
describe the intended architecture, while this file tracks the remaining gaps
that still need engineering evidence or follow-up code.

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

## Remaining Gaps

| Area | Gap | Risk | Next Work |
| --- | --- | --- | --- |
| Real hardware evidence | The local matrix covers unit and scale proxies, but production claims still require current HDD and SSD/NVMe hardware reports. | Scheduler and elevator tuning may regress on real rotational or network storage while proxy tests remain green. | Run `scripts/storage_hardware_matrix.sh /mnt/nvme /mnt/hdd` with `TNG_STORAGE_REQUIRE_HDD_5X=1`; keep the generated reports for release evidence. |
| `io_uring` fixed buffers | `UringBackend` uses worker-owned fixed buffers when available, but the global frame pool does not yet hand out stable registered buffer slots. | Extra copies remain in the uring path, and fixed-buffer metrics can overstate how much of the full storage path is zero-copy. | Add frame-pool slot leases, wire them through backend requests, and benchmark `pread` vs `uring` on real devices before making `uring` the `auto` default. |
| Hot-torrent memory estimates | Hot-torrent attribution uses coarse fixed multipliers for tracker peers, command queues, and cached storage handles. | The ranking is useful for triage but not exact enough for precise per-torrent accounting. | Replace constants with actual allocation sizes where local structures can report them cheaply. |
| Multi-root / multi-device scheduling | Topology detection and device ids exist, but `MountScheduler` remains rooted per mount and only peer-read HDD elevator is wired. | Cross-mount or cross-device fairness is not centrally enforced for all I/O classes. | Introduce a process-level device scheduler registry if real workloads show per-mount schedulers competing on the same spindle. |
| Move/import executor | Planning, conflict detection, and safe paths exist, but this remains outside the per-block hot path and needs separate certification when storage moves become release-critical. | Large library moves can still be operationally risky without full end-to-end soak evidence. | Run dedicated move/import certification on representative multi-TB trees and publish rollback/failure reports. |

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
```

Use these before claiming production storage performance:

```sh
TNG_STORAGE_REQUIRE_HDD_5X=1 scripts/storage_hardware_matrix.sh /mnt/nvme /mnt/hdd
```
