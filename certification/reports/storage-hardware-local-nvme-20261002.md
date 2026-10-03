# TorrentNG Storage Hardware Matrix

- Generated: 2026-10-02T21:16:21Z
- Host: kspld0
- Commit: db4e333c
- Worktree state: dirty
- Blocks: 1024
- Hot reads: 20000
- Syscall counts: 0

## /mnt/disks/gamespool1

| Field | Value |
| --- | --- |
| mount source | /dev/sdb |
| filesystem | btrfs |
| root block | /dev/sdb |
| physical device type | disk |
| rotational | 0 |
| inferred profile | SSD/NVMe |
| hardware qualification | PASS |
| qualification detail | block device and filesystem identified |
| HDD 5x gate | not enforced |

- Result: PASS

Summary:

    test backend_selection_roundtrip_reports_capabilities ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-DwKAnz profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
    tng_storage_backend requested=pread selected=pread reason="forced by storage backend configuration" fixed_buffers=false fixed_buffer_strategy=disabled registered_files=false max_batch_len=1 fixed_buffer_len=0
    test backend_selection_roundtrip_reports_capabilities ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-d0l1Wj profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
    tng_storage_backend requested=uring selected=uring reason="io_uring probe succeeded; registered_files=true fixed_buffers=true" fixed_buffers=true fixed_buffer_strategy=frame_pool_slots registered_files=true max_batch_len=64 fixed_buffer_len=262144
    test peer_read_readahead_reduces_backend_reads_on_adjacent_blocks ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-4AF80U profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
    tng_storage_readahead submitted=1024 backend_reads=32 reduction=32.00x elapsed_ms=26
    test repeated_reads_reuse_one_open_file_handle ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-rAguWf profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
    tng_storage_file_pool reads=20000 hits=19999 misses=1 open_files=1 capacity=64
    test recheck_range_reports_runtime_progress ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-tOMylN profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
    tng_storage_recheck pieces=1024 piece_len=16384 total_mib=16.00 valid=1024 invalid=0 missing=0 first_missing=None elapsed_ms=38 mib_s=419.04 read_ops=1024 backend_reads=1024 hash_ops=1024 hash_latency_ms=8
    test shuffled_peer_read_baseline_reports_current_scheduler_throughput ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-XKvrrf profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
    tng_storage_shuffled_baseline blocks=1024 block_len=16384 total_mib=16.00 elapsed_ms=30 mib_s=521.92 read_ops=1024 backend_reads=1024
    test hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-xXU93o profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
    tng_storage_elevator skipped_non_hdd_profile=Ssd
    TorrentNG storage elevator wall-clock ratio unavailable; likely skipped on non-HDD topology

## Gate

PASS

Overall status: PASS
