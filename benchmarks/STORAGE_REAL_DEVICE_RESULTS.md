# Storage Real-Device Results

Hardware validation for `scripts/storage_real_device_benchmark.sh`.

Command shape:

```sh
TNG_STORAGE_BENCH_BLOCKS=16384 \
TNG_STORAGE_BENCH_READS=100000 \
scripts/storage_real_device_benchmark.sh <path>
```

| Date | Host | Path | Profile | FS | CoW | Peer reads | Backend reads | Backend reduction | Shuffled baseline | File-pool reads | File-pool misses |
| --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: | --- | ---: | ---: |
| 2026-05-17 | local | `/home/keith/Documents/code` | `Nvme` | btrfs | yes | 512 | 65 | 7.88x | 8 MiB in 3 ms, 512 backend reads | 1,000 | 1 |
| 2026-05-17 | `kspls0` | `/mnt/datapool_lvm_media` | `Hdd` | ext4 | no | 16,384 | 2,034 | 8.06x | 256 MiB in 3,069 ms, 16,384 backend reads, 83.39 MiB/s | 100,000 | 1 |
| 2026-05-17 | `kspls0` | `/mnt/datapool_lvm_media` | `Hdd` | ext4 | no | 16,384 shuffled elevator | 1 | 16,384x | serial run: baseline 256 MiB in 10,280 ms, 24.90 MiB/s; elevator 256 MiB in 2,038 ms, 125.55 MiB/s; 5.04x wall-clock | 100,000 | 1 |

Notes:

- The current renamed mainline scheduler removes fd churn and gets an 8x
  backend-read reduction for adjacent peer reads through readahead.
- The HDD elevator run disables readahead, submits shuffled adjacent blocks,
  and proves `MountScheduler` can batch them into near-sequential backend
  reads on rotational storage. The serial HDD run clears the 5x wall-clock
  target with `TNG_STORAGE_REQUIRE_5X=1`.
