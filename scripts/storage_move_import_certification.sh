#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/storage-move-import-$(date -u +%Y%m%dT%H%M%SZ).md}"

mkdir -p "$(dirname "$OUT")"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

{
  echo "# TorrentNG Storage Move/Import Certification"
  echo
  echo "- Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo
  echo "| Gate | Result |"
  echo "| --- | --- |"
} >"$OUT"

run_gate() {
  local name="$1"
  shift
  local slug
  local log
  slug="$(printf '%s' "$name" | tr -c 'A-Za-z0-9_.-' '_')"
  log="$tmpdir/$slug.log"
  if "$@" >"$log" 2>&1; then
    echo "| $name | PASS |" >>"$OUT"
    append_gate_log "$name" "$log"
  else
    echo "| $name | FAIL |" >>"$OUT"
    append_gate_log "$name" "$log"
    return 1
  fi
}

append_gate_log() {
  local name="$1"
  local log="$2"
  {
    echo
    echo "## $name"
    echo
    echo '```text'
    cat "$log"
    echo '```'
  } >>"$OUT"
}

overall=0
run_gate "storage planner/executor unit tests" cargo test -p rt-storage plan::tests || overall=1
run_gate "full storage unit suite" cargo test -p rt-storage || overall=1

{
  echo
  echo "## Notes"
  echo
  echo "- Covers no-overwrite move execution, hardlink-or-copy import, recursive directory copy verification, staged rollback cleanup, and approved directory delete."
  echo "- Representative multi-TB operator soak should still use this report alongside hardware-specific move/import runs on the target storage roots."
} >>"$OUT"

echo "storage move/import report: $OUT"
exit "$overall"
