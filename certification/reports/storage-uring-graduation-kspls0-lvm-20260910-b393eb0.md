# TorrentNG io_uring Graduation Report

- Generated: 2026-09-10T17:53:39Z
- Host: kspls0
- Commit: b393eb0
- Target: /mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0
- Blocks: 1024
- Block length: 262144

## pread

- Result: PASS
- Selected: pread
- Read MiB/s: 209.79
- Write MiB/s: 185.82
- Fixed buffers: false
- Fixed-buffer strategy: disabled
- Registered files: false

```text
    Finished `release` profile [optimized] target(s) in 0.09s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test backend_stream_roundtrip_reports_throughput ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-OK9Hfk profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_backend_stream requested=pread selected=pread reason="forced by storage backend configuration" blocks=1024 block_len=262144 total_mib=256.00 write_elapsed_ms=1377 read_elapsed_ms=1220 write_mib_s=185.82 read_mib_s=209.79 fixed_buffers=false fixed_buffer_strategy=disabled registered_files=false max_batch_len=1 fixed_buffer_len=0
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 3.26s

```

## uring

- Result: PASS
- Selected: uring
- Read MiB/s: 210.70
- Write MiB/s: 181.50
- Fixed buffers: true
- Fixed-buffer strategy: frame_pool_slots
- Registered files: true

```text
    Finished `release` profile [optimized] target(s) in 0.08s
     Running tests/storage_real_device.rs (target/release/deps/storage_real_device-ae68b43ccca527e0)

running 1 test
test backend_stream_roundtrip_reports_throughput ... tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/tng-storage-bench-bTHjfy profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_backend_stream requested=uring selected=uring reason="io_uring probe succeeded; registered_files=true fixed_buffers=true" blocks=1024 block_len=262144 total_mib=256.00 write_elapsed_ms=1410 read_elapsed_ms=1214 write_mib_s=181.50 read_mib_s=210.70 fixed_buffers=true fixed_buffer_strategy=frame_pool_slots registered_files=true max_batch_len=64 fixed_buffer_len=262144
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 3.26s

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
