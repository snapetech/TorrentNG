#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/benchmarks/report-$(date -u +%Y%m%dT%H%M%SZ).md}"
BENCH_TORRENTS="${TNG_BENCH_TORRENTS:-}"

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
  echo "## Informational deterministic performance checks"
  echo
  echo "- This report is diagnostic only; it is not a release gate or torrent-count capacity certification."
  echo
  echo '```text'
} > "$OUT"

if [[ -n "$BENCH_TORRENTS" ]]; then
  echo "- An explicit bounded fixture size was supplied through TNG_BENCH_TORRENTS." >> "$OUT"
  (cd "$ROOT/sidecar" && TNG_BENCH_TORRENTS="$BENCH_TORRENTS" \
    cargo test --release --test benchmarks -- --ignored --nocapture) 2>&1 | tee -a "$OUT"
else
  (cd "$ROOT/sidecar" && cargo test --release --test benchmarks -- --ignored --nocapture) \
    2>&1 | tee -a "$OUT"
fi

{
  echo '```'
  echo
  echo "## Interpretation"
  echo
  echo "- Use the output to diagnose deterministic API regressions."
  echo "- Keep numeric capacity qualification and long-duration stability testing out of this report; those are intentionally deferred from the current release scope."
} >> "$OUT"

echo "$OUT"
