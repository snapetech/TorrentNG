#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/benchmarks/report-$(date -u +%Y%m%dT%H%M%SZ).md}"
BENCH_COUNTS="${TNG_BENCH_COUNTS:-1000 10000 15000 50000}"

mkdir -p "$(dirname "$OUT")"

{
  echo "# TorrentNG Benchmark Report"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Kernel: $(uname -srmo)"
  echo "- Rust: $(rustc --version 2>/dev/null || echo unavailable)"
  echo "- Cargo: $(cargo --version 2>/dev/null || echo unavailable)"
  echo
  echo "## Synthetic Benchmarks"
  echo
  echo "- Torrent counts: $BENCH_COUNTS"
  echo
  echo '```text'
} > "$OUT"

for count in $BENCH_COUNTS; do
  {
    echo
    echo "### TNG_BENCH_TORRENTS=$count"
  } | tee -a "$OUT"

  (
    cd "$ROOT/sidecar"
    TNG_BENCH_TORRENTS="$count" cargo test --release --test benchmarks -- --ignored --nocapture
  ) 2>&1 | tee -a "$OUT"
done

{
  echo '```'
  echo
  echo "## Release Targets"
  echo
  echo "| Scenario | Target |"
  echo "|---|---|"
  echo "| 50k synthetic qBit torrents/info | < 500ms |"
  echo "| qBit sync/maindata delta | < 50ms |"
  echo "| 15k memory soak | < 500MB after 24h |"
  echo "| Cold start first list | < 5s |"
} >> "$OUT"

echo "$OUT"
