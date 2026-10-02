# Benchmarks

Compatible-client service benchmarks that can run without a live rTorrent
client live in `sidecar/tests/benchmarks.rs`. The source path is retained for
repository compatibility.

Run the synthetic API checks explicitly:

```sh
cd sidecar
cargo test --test benchmarks -- --ignored --nocapture
```

Use a bounded local fixture while diagnosing a regression:

Set `TNG_BENCH_TORRENTS` to a bounded fixture size while diagnosing a
regression, then run the same command above.

Generate a markdown report:

```sh
./scripts/benchmark_report.sh
```

The report runner uses `cargo test --release` because optimized behavior is
useful for diagnosis. The generated report is informational and is not a
release gate or torrent-count capacity certificate.

Current covered regression surfaces:

- qBit `/api/qb/v2/torrents/info` serialization and pagination
- qBit `/api/qb/v2/sync/maindata` delta under normal churn

Storage hardware checks live in `rt-storage` and can be run against a specific
mount:

```sh
scripts/storage_real_device_benchmark.sh /path/on/storage
```

Recent hardware results are tracked in
`benchmarks/STORAGE_REAL_DEVICE_RESULTS.md`.
