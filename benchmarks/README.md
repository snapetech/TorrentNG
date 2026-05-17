# Benchmarks

Track 1 roadmap benchmarks that can run without a live rTorrent instance live in
`sidecar/tests/benchmarks.rs`.

Run the synthetic API checks explicitly:

```sh
cd sidecar
cargo test --test benchmarks -- --ignored --nocapture
```

Use a smaller local dataset while iterating:

```sh
TNG_BENCH_TORRENTS=10000 cargo test --test benchmarks -- --ignored --nocapture
```

Generate a markdown report:

```sh
./scripts/benchmark_report.sh
```

The report runner uses `cargo test --release` because the roadmap targets are release-build targets. Debug builds are useful for correctness but are not representative for the 50k-row JSON/API path.

Current covered targets:

- qBit `/api/qb/v2/torrents/info` at 50k synthetic torrents: `< 500ms`
- qBit `/api/qb/v2/sync/maindata` delta under normal churn: `< 50ms`

Storage hardware checks live in `rt-storage` and can be run against a specific
mount:

```sh
scripts/storage_real_device_benchmark.sh /path/on/storage
```
