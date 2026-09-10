# TorrentNG Storage Release Certification

- Generated: 2026-09-10T17:53:08Z
- Host: kspls0
- Commit: b393eb0
- Targets: /mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0

| Gate | Status | Detail |
| --- | --- | --- |
| storage hardware matrix | PASS | storage hardware report: /tmp/torrentng-674b283-20260910-v4/certification/reports/storage-hardware-kspls0-lvm-20260910-b393eb0.md |

## storage hardware matrix

```text
== TorrentNG storage hardware matrix: /mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0 (HDD) ==
TorrentNG storage benchmark dir: /mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0
Backing source: /dev/mapper/datapool_lvm-media
Rotational flag: 1
Syscall summary: 0

==> backend_selection_roundtrip_reports_capabilities backend=pread
    Finished `release` profile [optimized] target(s) in 0.21s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test backend_selection_roundtrip_reports_capabilities ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-eT3h4Z profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_backend requested=pread selected=pread reason="forced by storage backend configuration" fixed_buffers=false fixed_buffer_strategy=disabled registered_files=false max_batch_len=1 fixed_buffer_len=0
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.01s


==> backend_selection_roundtrip_reports_capabilities backend=uring
    Finished `release` profile [optimized] target(s) in 0.12s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test backend_selection_roundtrip_reports_capabilities ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-breHjw profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_backend requested=uring selected=uring reason="io_uring probe succeeded; registered_files=true fixed_buffers=true" fixed_buffers=true fixed_buffer_strategy=frame_pool_slots registered_files=true max_batch_len=64 fixed_buffer_len=262144
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.01s


==> peer_read_readahead_reduces_backend_reads_on_adjacent_blocks
    Finished `release` profile [optimized] target(s) in 0.10s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test peer_read_readahead_reduces_backend_reads_on_adjacent_blocks ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-joRhl3 profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_readahead submitted=8192 backend_reads=256 reduction=32.00x elapsed_ms=664
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 1.79s


==> repeated_reads_reuse_one_open_file_handle
    Finished `release` profile [optimized] target(s) in 0.15s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test repeated_reads_reuse_one_open_file_handle ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-qcCBAA profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_file_pool reads=10000 hits=9999 misses=1 open_files=1 capacity=64
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.30s


==> recheck_range_reports_runtime_progress
    Finished `release` profile [optimized] target(s) in 0.10s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test recheck_range_reports_runtime_progress ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-WL2kGE profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_recheck pieces=4096 piece_len=16384 total_mib=64.00 valid=4096 invalid=0 missing=0 first_missing=None elapsed_ms=331 mib_s=193.34 read_ops=4096 backend_reads=4096 hash_ops=4096 hash_latency_ms=59
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.87s


==> shuffled_peer_read_baseline_reports_current_scheduler_throughput
    Finished `release` profile [optimized] target(s) in 0.10s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test shuffled_peer_read_baseline_reports_current_scheduler_throughput ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-sMRmu8 profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_shuffled_baseline blocks=8192 block_len=16384 total_mib=128.00 elapsed_ms=3769 mib_s=33.96 read_ops=8192 backend_reads=8192
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 4.91s


==> hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks
    Finished `release` profile [optimized] target(s) in 0.13s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-yzjGpE profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_elevator blocks=8192 block_len=16384 total_mib=128.00 elapsed_ms=726 mib_s=176.12 submitted=8192 backend_reads=1 reduction=8192.00x batches=1 coalesced=8191
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 1.64s

TorrentNG storage elevator trial 1/3: 5.19x baseline/elevator (3769ms/726ms)

==> shuffled_peer_read_baseline_reports_current_scheduler_throughput
    Finished `release` profile [optimized] target(s) in 0.09s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test shuffled_peer_read_baseline_reports_current_scheduler_throughput ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-uxzQvv profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_shuffled_baseline blocks=8192 block_len=16384 total_mib=128.00 elapsed_ms=3458 mib_s=37.01 read_ops=8192 backend_reads=8192
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 4.40s


==> hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks
    Finished `release` profile [optimized] target(s) in 0.11s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-goOIzr profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_elevator blocks=8192 block_len=16384 total_mib=128.00 elapsed_ms=706 mib_s=181.11 submitted=8192 backend_reads=1 reduction=8192.00x batches=1 coalesced=8191
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 1.62s

TorrentNG storage elevator trial 2/3: 4.90x baseline/elevator (3458ms/706ms)

==> shuffled_peer_read_baseline_reports_current_scheduler_throughput
    Finished `release` profile [optimized] target(s) in 0.08s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test shuffled_peer_read_baseline_reports_current_scheduler_throughput ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-h1Yy32 profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_shuffled_baseline blocks=8192 block_len=16384 total_mib=128.00 elapsed_ms=3767 mib_s=33.98 read_ops=8192 backend_reads=8192
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 4.70s


==> hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks
    Finished `release` profile [optimized] target(s) in 0.14s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-iqkHjc profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_elevator blocks=8192 block_len=16384 total_mib=128.00 elapsed_ms=737 mib_s=173.51 submitted=8192 backend_reads=1 reduction=8192.00x batches=1 coalesced=8191
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 1.76s

TorrentNG storage elevator trial 3/3: 5.11x baseline/elevator (3767ms/737ms)

TorrentNG storage elevator wall-clock ratio: 5.11x median baseline/elevator (3767ms/726ms across 3 trial(s))
storage hardware report: /tmp/torrentng-674b283-20260910-v4/certification/reports/storage-hardware-kspls0-lvm-20260910-b393eb0.md
```
| io_uring graduation | PASS | storage uring graduation report: /tmp/torrentng-674b283-20260910-v4/certification/reports/storage-uring-graduation-kspls0-lvm-20260910-b393eb0.md |

## io_uring graduation

```text
== TorrentNG backend stream: pread ==
    Finished `release` profile [optimized] target(s) in 0.09s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test backend_stream_roundtrip_reports_throughput ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-OK9Hfk profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_backend_stream requested=pread selected=pread reason="forced by storage backend configuration" blocks=1024 block_len=262144 total_mib=256.00 write_elapsed_ms=1377 read_elapsed_ms=1220 write_mib_s=185.82 read_mib_s=209.79 fixed_buffers=false fixed_buffer_strategy=disabled registered_files=false max_batch_len=1 fixed_buffer_len=0
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 3.26s

== TorrentNG backend stream: uring ==
    Finished `release` profile [optimized] target(s) in 0.08s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test backend_stream_roundtrip_reports_throughput ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-bTHjfy profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_backend_stream requested=uring selected=uring reason="io_uring probe succeeded; registered_files=true fixed_buffers=true" blocks=1024 block_len=262144 total_mib=256.00 write_elapsed_ms=1410 read_elapsed_ms=1214 write_mib_s=181.50 read_mib_s=210.70 fixed_buffers=true fixed_buffer_strategy=frame_pool_slots registered_files=true max_batch_len=64 fixed_buffer_len=262144
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 3.26s

storage uring graduation report: /tmp/torrentng-674b283-20260910-v4/certification/reports/storage-uring-graduation-kspls0-lvm-20260910-b393eb0.md
```
| real-root move/import | PASS | storage move/import report: /tmp/torrentng-674b283-20260910-v4/certification/reports/storage-move-import-kspls0-lvm-20260910-b393eb0.md |

## real-root move/import

```text
storage move/import report: /tmp/torrentng-674b283-20260910-v4/certification/reports/storage-move-import-kspls0-lvm-20260910-b393eb0.md
```
| storage certification index | PASS | storage certification index: /tmp/torrentng-674b283-20260910-v4/certification/reports/storage-certification-index.md |

## storage certification index

```text
storage certification index: /tmp/torrentng-674b283-20260910-v4/certification/reports/storage-certification-index.md
```

## Boundaries

- This script runs destructive-safe fixtures only; it creates and removes its own test files under the selected roots.
- Physical PV affinity remains evidence-only because ordinary LV path writes do not select a specific PV.
-  and  fail release reports unless  is also set for an explicit dry run.

Overall status: PASS
