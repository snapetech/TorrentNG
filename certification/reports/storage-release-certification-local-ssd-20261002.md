# TorrentNG Storage Release Certification

- Generated: 2026-10-02T21:16:23Z
- Host: kspld0
- Commit: db4e333c
- Worktree state: dirty
- Targets: /mnt/disks/gamespool1

| Gate | Status | Detail |
| --- | --- | --- |
| storage hardware matrix | PASS | physical-device qualification passed; smoke-only targets remain labeled |

## storage hardware matrix

```text
== TorrentNG storage hardware matrix: /mnt/disks/gamespool1 (SSD/NVMe) ==
TorrentNG storage benchmark dir: /mnt/disks/gamespool1
Backing source: /dev/sdb
Rotational flag: 0
Syscall summary: 0

==> backend_selection_roundtrip_reports_capabilities backend=pread
    Finished `release` profile [optimized] target(s) in 0.04s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-101fde4519a02d3a)

running 1 test
test backend_selection_roundtrip_reports_capabilities ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-0MVvoa profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
tng_storage_backend requested=pread selected=pread reason="forced by storage backend configuration" fixed_buffers=false fixed_buffer_strategy=disabled registered_files=false max_batch_len=1 fixed_buffer_len=0
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s


==> backend_selection_roundtrip_reports_capabilities backend=uring
    Finished `release` profile [optimized] target(s) in 0.04s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-101fde4519a02d3a)

running 1 test
test backend_selection_roundtrip_reports_capabilities ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-DHS5cV profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
tng_storage_backend requested=uring selected=uring reason="io_uring probe succeeded; registered_files=true fixed_buffers=true" fixed_buffers=true fixed_buffer_strategy=frame_pool_slots registered_files=true max_batch_len=64 fixed_buffer_len=262144
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s


==> peer_read_readahead_reduces_backend_reads_on_adjacent_blocks
    Finished `release` profile [optimized] target(s) in 0.04s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-101fde4519a02d3a)

running 1 test
test peer_read_readahead_reduces_backend_reads_on_adjacent_blocks ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-kpCz58 profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
tng_storage_readahead submitted=1024 backend_reads=32 reduction=32.00x elapsed_ms=29
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.06s


==> repeated_reads_reuse_one_open_file_handle
    Finished `release` profile [optimized] target(s) in 0.04s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-101fde4519a02d3a)

running 1 test
test repeated_reads_reuse_one_open_file_handle ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-ckF3pb profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
tng_storage_file_pool reads=20000 hits=19999 misses=1 open_files=1 capacity=64
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.22s


==> recheck_range_reports_runtime_progress
    Finished `release` profile [optimized] target(s) in 0.04s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-101fde4519a02d3a)

running 1 test
test recheck_range_reports_runtime_progress ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-uTbxgj profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
tng_storage_recheck pieces=1024 piece_len=16384 total_mib=16.00 valid=1024 invalid=0 missing=0 first_missing=None elapsed_ms=41 mib_s=382.01 read_ops=1024 backend_reads=1024 hash_ops=1024 hash_latency_ms=9
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.08s


==> shuffled_peer_read_baseline_reports_current_scheduler_throughput
    Finished `release` profile [optimized] target(s) in 0.04s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-101fde4519a02d3a)

running 1 test
test shuffled_peer_read_baseline_reports_current_scheduler_throughput ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-N5RfOr profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
tng_storage_shuffled_baseline blocks=1024 block_len=16384 total_mib=16.00 elapsed_ms=34 mib_s=462.20 read_ops=1024 backend_reads=1024
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.07s


==> hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks
    Finished `release` profile [optimized] target(s) in 0.04s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-101fde4519a02d3a)

running 1 test
test hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-XuTFiX profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
tng_storage_elevator skipped_non_hdd_profile=Ssd
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s

TorrentNG storage elevator trial 1/1: ratio unavailable

TorrentNG storage elevator wall-clock ratio unavailable; likely skipped on non-HDD topology
storage hardware report: /home/keith/Documents/code/TorrentNG/certification/reports/storage-hardware-release-20261002T211623Z.md
```
| io_uring graduation | PASS | storage uring graduation report: /home/keith/Documents/code/TorrentNG/certification/reports/storage-uring-graduation-release-20261002T211623Z.md |

## io_uring graduation

```text
== TorrentNG backend stream: pread ==
    Finished `release` profile [optimized] target(s) in 0.04s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-101fde4519a02d3a)

running 1 test
test backend_stream_roundtrip_reports_throughput ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-tw6xZc profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
tng_storage_backend_stream requested=pread selected=pread reason="forced by storage backend configuration" blocks=1024 block_len=262144 total_mib=256.00 write_elapsed_ms=377 read_elapsed_ms=236 write_mib_s=678.24 read_mib_s=1082.36 fixed_buffers=false fixed_buffer_strategy=disabled registered_files=false max_batch_len=1 fixed_buffer_len=0
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.70s

== TorrentNG backend stream: uring ==
    Finished `release` profile [optimized] target(s) in 0.04s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-101fde4519a02d3a)

running 1 test
test backend_stream_roundtrip_reports_throughput ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-ro8MUP profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
tng_storage_backend_stream requested=uring selected=uring reason="io_uring probe succeeded; registered_files=true fixed_buffers=true" blocks=1024 block_len=262144 total_mib=256.00 write_elapsed_ms=479 read_elapsed_ms=236 write_mib_s=534.15 read_mib_s=1081.92 fixed_buffers=true fixed_buffer_strategy=frame_pool_slots registered_files=true max_batch_len=64 fixed_buffer_len=262144
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.81s

storage uring graduation report: /home/keith/Documents/code/TorrentNG/certification/reports/storage-uring-graduation-release-20261002T211623Z.md
```
| real-root move/import | PASS | storage move/import report: /home/keith/Documents/code/TorrentNG/certification/reports/storage-move-import-release-20261002T211623Z.md |

## real-root move/import

```text
storage move/import report: /home/keith/Documents/code/TorrentNG/certification/reports/storage-move-import-release-20261002T211623Z.md
```
| storage certification index | PASS | storage certification index: /home/keith/Documents/code/TorrentNG/certification/reports/storage-certification-index.md |

## storage certification index

```text
storage certification index: /home/keith/Documents/code/TorrentNG/certification/reports/storage-certification-index.md
```

## Boundaries

- This script runs destructive-safe fixtures only; it creates and removes its own test files under the selected roots.
- Physical PV affinity remains evidence-only because ordinary LV path writes do not select a specific PV.
- `TNG_STORAGE_SKIP_URING=1` and `TNG_STORAGE_SKIP_MOVE_IMPORT=1` fail release reports unless `TNG_STORAGE_ALLOW_RELEASE_SKIP=1` is also set for an explicit dry run.

Overall status: PASS
