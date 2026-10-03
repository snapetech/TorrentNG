# TorrentNG io_uring Graduation Report

- Generated: 2026-10-02T21:16:25Z
- Host: kspld0
- Commit: db4e333c
- Target: /mnt/disks/gamespool1
- Blocks: 1024
- Block length: 262144

## pread

- Result: PASS
- Selected: pread
- Read MiB/s: 1082.36
- Write MiB/s: 678.24
- Fixed buffers: false
- Fixed-buffer strategy: disabled
- Registered files: false

```text
    Finished `release` profile [optimized] target(s) in 0.04s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-101fde4519a02d3a)

running 1 test
test backend_stream_roundtrip_reports_throughput ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-tw6xZc profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
tng_storage_backend_stream requested=pread selected=pread reason="forced by storage backend configuration" blocks=1024 block_len=262144 total_mib=256.00 write_elapsed_ms=377 read_elapsed_ms=236 write_mib_s=678.24 read_mib_s=1082.36 fixed_buffers=false fixed_buffer_strategy=disabled registered_files=false max_batch_len=1 fixed_buffer_len=0
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.70s

```

## uring

- Result: PASS
- Selected: uring
- Read MiB/s: 1081.92
- Write MiB/s: 534.15
- Fixed buffers: true
- Fixed-buffer strategy: frame_pool_slots
- Registered files: true

```text
    Finished `release` profile [optimized] target(s) in 0.04s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-101fde4519a02d3a)

running 1 test
test backend_stream_roundtrip_reports_throughput ... tng_storage_bench_path=/mnt/disks/gamespool1/tng-storage-bench-ro8MUP profile=Ssd fs=Some("btrfs") cow=true device=Some(DeviceId("sdb"))
tng_storage_backend_stream requested=uring selected=uring reason="io_uring probe succeeded; registered_files=true fixed_buffers=true" blocks=1024 block_len=262144 total_mib=256.00 write_elapsed_ms=479 read_elapsed_ms=236 write_mib_s=534.15 read_mib_s=1081.92 fixed_buffers=true fixed_buffer_strategy=frame_pool_slots registered_files=true max_batch_len=64 fixed_buffer_len=262144
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.81s

```

## Graduation Gates

| Gate | Result |
| --- | --- |
| uring selected | PASS |
| fixed buffers | INFO: true |
| fixed-buffer strategy frame_pool_slots | PASS |
| registered files | INFO: true |
| read throughput ratio | INFO: not required |
| write throughput ratio | INFO: not required |

Overall status: PASS
