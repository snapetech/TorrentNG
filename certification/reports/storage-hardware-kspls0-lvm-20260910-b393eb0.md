# TorrentNG Storage Hardware Matrix

- Generated: 2026-09-10T17:53:08Z
- Host: kspls0
- Commit: b393eb0
- Blocks: 8192
- Hot reads: 10000
- Syscall counts: 0

## /mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0

| Field | Value |
| --- | --- |
| mount source | /dev/mapper/datapool_lvm-media |
| filesystem | ext4 |
| root block | /dev/dm-0 |
| rotational | 1 |
| inferred profile | HDD |

LVM/PV extent probe:

| File | Extent | LV sector | Sectors | PV | PV sector | Rotational |
| --- | ---: | ---: | ---: | --- | ---: | ---: |
| probe-0.bin | 0 | 172258492416 | 262144 | /dev/sde | 19910912000 | 1 |
| probe-0.bin | 1 | 172258754560 | 262144 | /dev/sde | 19911174144 | 1 |
| probe-1.bin | 0 | 194915565568 | 16384 | /dev/sdf | 11316234240 | 1 |
| probe-1.bin | 1 | 194920005632 | 16384 | /dev/sdf | 11320674304 | 1 |
| probe-1.bin | 2 | 194927771648 | 16384 | /dev/sdf | 11328440320 | 1 |
| probe-1.bin | 3 | 194927919104 | 16384 | /dev/sdf | 11328587776 | 1 |
| probe-1.bin | 4 | 194928574464 | 16384 | /dev/sdf | 11329243136 | 1 |
| probe-1.bin | 5 | 194929426432 | 16384 | /dev/sdf | 11330095104 | 1 |
| probe-1.bin | 6 | 194935341056 | 16384 | /dev/sdf | 11336009728 | 1 |
| probe-1.bin | 7 | 194936061952 | 16384 | /dev/sdf | 11336730624 | 1 |
| probe-1.bin | 8 | 194936291328 | 16384 | /dev/sdf | 11336960000 | 1 |
| probe-1.bin | 9 | 194937536512 | 16384 | /dev/sdf | 11338205184 | 1 |
| probe-1.bin | 10 | 194939682816 | 16384 | /dev/sdf | 11340351488 | 1 |
| probe-1.bin | 11 | 194946957312 | 16384 | /dev/sdf | 11347625984 | 1 |
| probe-1.bin | 12 | 194947776512 | 16384 | /dev/sdf | 11348445184 | 1 |
| probe-1.bin | 13 | 194950316032 | 16384 | /dev/sdf | 11350984704 | 1 |
| probe-1.bin | 14 | 194952019968 | 16384 | /dev/sdf | 11352688640 | 1 |
| probe-1.bin | 15 | 194952511488 | 16384 | /dev/sdf | 11353180160 | 1 |
| probe-1.bin | 16 | 194952675328 | 16384 | /dev/sdf | 11353344000 | 1 |
| probe-1.bin | 17 | 194954231808 | 16384 | /dev/sdf | 11354900480 | 1 |
| probe-1.bin | 18 | 194954280960 | 16384 | /dev/sdf | 11354949632 | 1 |
| probe-1.bin | 19 | 194956836864 | 16384 | /dev/sdf | 11357505536 | 1 |
| probe-1.bin | 20 | 194960228352 | 16384 | /dev/sdf | 11360897024 | 1 |
| probe-1.bin | 21 | 194960932864 | 32768 | /dev/sdf | 11361601536 | 1 |
| probe-1.bin | 22 | 194961719296 | 131072 | /dev/sdf | 11362387968 | 1 |
| probe-1.bin | 23 | 194962046976 | 16384 | /dev/sdf | 11362715648 | 1 |
| probe-2.bin | 0 | 496874848256 | 16384 | /dev/sdj | 16394348544 | 1 |
| probe-2.bin | 1 | 496875159552 | 49152 | /dev/sdj | 16394659840 | 1 |
| probe-2.bin | 2 | 496875077632 | 32768 | /dev/sdj | 16394577920 | 1 |
| probe-2.bin | 3 | 496875454464 | 98304 | /dev/sdj | 16394954752 | 1 |
| probe-2.bin | 4 | 496875569152 | 49152 | /dev/sdj | 16395069440 | 1 |
| probe-2.bin | 5 | 496875634688 | 16384 | /dev/sdj | 16395134976 | 1 |

- Result: PASS

Summary:

    test backend_selection_roundtrip_reports_capabilities ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-eT3h4Z profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
    tng_storage_backend requested=pread selected=pread reason="forced by storage backend configuration" fixed_buffers=false fixed_buffer_strategy=disabled registered_files=false max_batch_len=1 fixed_buffer_len=0
    test backend_selection_roundtrip_reports_capabilities ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-breHjw profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
    tng_storage_backend requested=uring selected=uring reason="io_uring probe succeeded; registered_files=true fixed_buffers=true" fixed_buffers=true fixed_buffer_strategy=frame_pool_slots registered_files=true max_batch_len=64 fixed_buffer_len=262144
    test peer_read_readahead_reduces_backend_reads_on_adjacent_blocks ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-joRhl3 profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
    tng_storage_readahead submitted=8192 backend_reads=256 reduction=32.00x elapsed_ms=664
    test repeated_reads_reuse_one_open_file_handle ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-qcCBAA profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
    tng_storage_file_pool reads=10000 hits=9999 misses=1 open_files=1 capacity=64
    test recheck_range_reports_runtime_progress ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-WL2kGE profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
    tng_storage_recheck pieces=4096 piece_len=16384 total_mib=64.00 valid=4096 invalid=0 missing=0 first_missing=None elapsed_ms=331 mib_s=193.34 read_ops=4096 backend_reads=4096 hash_ops=4096 hash_latency_ms=59
    test shuffled_peer_read_baseline_reports_current_scheduler_throughput ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-sMRmu8 profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
    tng_storage_shuffled_baseline blocks=8192 block_len=16384 total_mib=128.00 elapsed_ms=3769 mib_s=33.96 read_ops=8192 backend_reads=8192
    test hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-yzjGpE profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
    tng_storage_elevator blocks=8192 block_len=16384 total_mib=128.00 elapsed_ms=726 mib_s=176.12 submitted=8192 backend_reads=1 reduction=8192.00x batches=1 coalesced=8191
    test shuffled_peer_read_baseline_reports_current_scheduler_throughput ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-uxzQvv profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
    tng_storage_shuffled_baseline blocks=8192 block_len=16384 total_mib=128.00 elapsed_ms=3458 mib_s=37.01 read_ops=8192 backend_reads=8192
    test hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-goOIzr profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
    tng_storage_elevator blocks=8192 block_len=16384 total_mib=128.00 elapsed_ms=706 mib_s=181.11 submitted=8192 backend_reads=1 reduction=8192.00x batches=1 coalesced=8191
    test shuffled_peer_read_baseline_reports_current_scheduler_throughput ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-h1Yy32 profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
    tng_storage_shuffled_baseline blocks=8192 block_len=16384 total_mib=128.00 elapsed_ms=3767 mib_s=33.98 read_ops=8192 backend_reads=8192
    test hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-iqkHjc profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
    tng_storage_elevator blocks=8192 block_len=16384 total_mib=128.00 elapsed_ms=737 mib_s=173.51 submitted=8192 backend_reads=1 reduction=8192.00x batches=1 coalesced=8191
    TorrentNG storage elevator wall-clock ratio: 5.11x median baseline/elevator (3767ms/726ms across 3 trial(s))

## Gate

PASS

Overall status: PASS
