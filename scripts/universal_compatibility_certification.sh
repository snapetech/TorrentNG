#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/universal-compat-$(date -u +%Y%m%dT%H%M%SZ).md}"

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
  echo "# TorrentNG Universal Compatibility Certification Report"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
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

run_gate "API facade endpoint and field matrices" "$ROOT/scripts/api_facade_certification.sh" "$ROOT/certification/reports/api-facades-universal-$(date -u +%Y%m%dT%H%M%SZ).md"
run_gate "migration dry-run, DB import, and fastresume matrices" cargo test -p rt-migrate
run_gate "native API compatibility manifest" cargo test -p rt-api-native
run_gate "native engine state, tracker, and storage hooks" cargo test -p rt-engine
run_gate "scale and metrics compatibility evidence" cargo test -p rt-metrics

sed -i "/|---|---|/r $OUT.table" "$OUT"
rm -f "$OUT.table"

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
