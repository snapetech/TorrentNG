#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/native-engine-$(date -u +%Y%m%dT%H%M%SZ).md}"

mkdir -p "$(dirname "$OUT")"

status="PASS"

run_gate() {
  local name="$1"
  shift
  {
    echo
    echo "## $name"
    echo
    echo '```text'
  } >> "$OUT"
  if (cd "$ROOT" && "$@") >> "$OUT" 2>&1; then
    echo '```' >> "$OUT"
    printf '| %s | PASS |\n' "$name" >> "$OUT.table"
  else
    echo '```' >> "$OUT"
    printf '| %s | FAIL |\n' "$name" >> "$OUT.table"
    status="FAIL"
  fi
}

{
  echo "# rtorrentNG Native Engine Certification Report"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Kernel: $(uname -srmo)"
  echo "- Rust: $(rustc --version 2>/dev/null || echo unavailable)"
  echo "- Cargo: $(cargo --version 2>/dev/null || echo unavailable)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo
  echo "## Gates"
  echo
  echo "| Gate | Result |"
  echo "|---|---|"
} > "$OUT"
: > "$OUT.table"

run_gate "migration dry-run and native DB import" cargo test -p rt-migrate
run_gate "scale and performance certification" cargo test -p rt-metrics
run_gate "compatibility API projections" cargo test -p rt-api-qbit -p rt-api-transmission -p rt-api-deluge
run_gate "native engine state and recovery" cargo test -p rt-engine

sed -i "/|---|---|/r $OUT.table" "$OUT"
rm -f "$OUT.table"

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
