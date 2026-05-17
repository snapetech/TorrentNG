#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/api-facades-$(date -u +%Y%m%dT%H%M%SZ).md}"

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
  echo "# TorrentNG API Facade Certification Report"
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

run_gate "qBittorrent Web API facade matrix" cargo test -p rt-api-qbit
run_gate "Transmission RPC facade matrix" cargo test -p rt-api-transmission
run_gate "Deluge JSON-RPC facade matrix" cargo test -p rt-api-deluge
run_gate "rTorrent XMLRPC facade matrix" cargo test -p rt-api-rtorrent

sed -i "/|---|---|/r $OUT.table" "$OUT"
rm -f "$OUT.table"

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
