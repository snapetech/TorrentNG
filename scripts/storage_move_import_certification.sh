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
  echo "- Hardware root: ${TNG_STORAGE_MOVE_IMPORT_ROOT:-not set}"
  echo "- Hardware files: ${TNG_STORAGE_MOVE_IMPORT_FILES:-64}"
  echo "- Hardware MiB/file: ${TNG_STORAGE_MOVE_IMPORT_MIB_PER_FILE:-1}"
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
if [[ -n "${TNG_STORAGE_MOVE_IMPORT_ROOT:-}" ]]; then
  run_gate "real-root move/import/delete executor" \
    cargo test -p rt-storage --test storage_move_import_hardware \
    move_import_delete_executor_runs_on_configured_storage_root -- --ignored --nocapture || overall=1
else
  {
    echo "| real-root move/import/delete executor | SKIP: set TNG_STORAGE_MOVE_IMPORT_ROOT |"
    echo
    echo "## real-root move/import/delete executor"
    echo
    echo "Skipped because TNG_STORAGE_MOVE_IMPORT_ROOT was not set."
  } >>"$OUT"
fi

{
  echo
  echo "## Notes"
  echo
  echo "- Covers no-overwrite move execution, copy-based move source cleanup after verified rename, hardlink-or-copy import, recursive directory copy verification, symlink rejection, staged rollback cleanup, approved directory delete, and storage-root confinement."
  echo "- Set TNG_STORAGE_MOVE_IMPORT_ROOT to run the same executor on a real storage root. Increase TNG_STORAGE_MOVE_IMPORT_FILES and TNG_STORAGE_MOVE_IMPORT_MIB_PER_FILE for larger operator soaks."
} >>"$OUT"

echo "storage move/import report: $OUT"
exit "$overall"
