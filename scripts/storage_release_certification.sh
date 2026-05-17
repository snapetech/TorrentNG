#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${TNG_STORAGE_REPORT_DIR:-$ROOT/certification/reports}"
OUT="${TNG_STORAGE_RELEASE_REPORT:-$REPORT_DIR/storage-release-certification-$(date -u +%Y%m%dT%H%M%SZ).md}"

usage() {
  cat >&2 <<'USAGE'
usage: scripts/storage_release_certification.sh /mount/or/path [...]

Runs the production Storage NG evidence suite and writes one rollup report:
  - storage hardware matrix across all target paths
  - pread vs io_uring graduation on one target path
  - real-root move/import/delete fixture on one target path
  - regenerated storage certification index

Environment:
  TNG_STORAGE_URING_TARGET       target path for io_uring graduation (default: first arg)
  TNG_STORAGE_MOVE_IMPORT_ROOT   target root for move/import fixture (default: first arg)
  TNG_STORAGE_SKIP_URING         set to 1 to skip io_uring graduation (FAIL unless TNG_STORAGE_ALLOW_RELEASE_SKIP=1)
  TNG_STORAGE_SKIP_MOVE_IMPORT   set to 1 to skip real-root move/import (FAIL unless TNG_STORAGE_ALLOW_RELEASE_SKIP=1)
  TNG_STORAGE_ALLOW_RELEASE_SKIP set to 1 to allow explicit SKIP rows for dry-run reports
  TNG_STORAGE_REQUIRE_HDD_5X     forwarded to storage_hardware_matrix.sh
  TNG_STORAGE_URING_*            forwarded to storage_uring_graduation.sh
  TNG_STORAGE_MOVE_IMPORT_*      forwarded to storage_move_import_certification.sh
  TNG_STORAGE_RELEASE_REPORT     report path override
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ "$#" -eq 0 ]]; then
  usage
  exit 2
fi

mkdir -p "$REPORT_DIR" "$(dirname "$OUT")"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

overall=0

append_log() {
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

mark() {
  local name="$1"
  local status="$2"
  local detail="$3"
  detail="${detail//$'\n'/ }"
  detail="${detail//|/\\|}"
  printf '| %s | %s | %s |\n' "$name" "$status" "$detail" >>"$OUT"
  case "$status" in
    PASS|INFO) ;;
    SKIP)
      if [[ "${TNG_STORAGE_ALLOW_RELEASE_SKIP:-0}" != "1" ]]; then
        overall=1
      fi
      ;;
    *) overall=1 ;;
  esac
}

run_gate() {
  local name="$1"
  shift
  local log="$tmpdir/$(printf '%s' "$name" | tr -c 'A-Za-z0-9_.-' '_').log"
  if "$@" >"$log" 2>&1; then
    mark "$name" PASS "$(tail -1 "$log")"
  else
    mark "$name" FAIL "$(tail -1 "$log")"
  fi
  append_log "$name" "$log"
}

{
  echo "# TorrentNG Storage Release Certification"
  echo
  echo "- Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "- Targets: $*"
  echo
  echo "| Gate | Status | Detail |"
  echo "| --- | --- | --- |"
} >"$OUT"

uring_target="${TNG_STORAGE_URING_TARGET:-$1}"
move_root="${TNG_STORAGE_MOVE_IMPORT_ROOT:-$1}"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"

run_gate "storage hardware matrix" env \
  TNG_STORAGE_MATRIX_REPORT="${TNG_STORAGE_MATRIX_REPORT:-$REPORT_DIR/storage-hardware-release-$stamp.md}" \
  "$ROOT/scripts/storage_hardware_matrix.sh" "$@"

if [[ "${TNG_STORAGE_SKIP_URING:-0}" == "1" ]]; then
  mark "io_uring graduation" SKIP "TNG_STORAGE_SKIP_URING=1"
else
  run_gate "io_uring graduation" env \
    TNG_STORAGE_URING_REQUIRE_SELECTED="${TNG_STORAGE_URING_REQUIRE_SELECTED:-1}" \
    TNG_STORAGE_URING_REQUIRE_FRAME_POOL_SLOTS="${TNG_STORAGE_URING_REQUIRE_FRAME_POOL_SLOTS:-1}" \
    TNG_STORAGE_URING_REPORT="${TNG_STORAGE_URING_REPORT:-$REPORT_DIR/storage-uring-graduation-release-$stamp.md}" \
    "$ROOT/scripts/storage_uring_graduation.sh" "$uring_target"
fi

if [[ "${TNG_STORAGE_SKIP_MOVE_IMPORT:-0}" == "1" ]]; then
  mark "real-root move/import" SKIP "TNG_STORAGE_SKIP_MOVE_IMPORT=1"
else
  run_gate "real-root move/import" env TNG_STORAGE_MOVE_IMPORT_ROOT="$move_root" \
    "$ROOT/scripts/storage_move_import_certification.sh" \
    "${TNG_STORAGE_MOVE_IMPORT_REPORT:-$REPORT_DIR/storage-move-import-release-$stamp.md}"
fi

run_gate "storage certification index" env \
  TNG_STORAGE_REPORT_DIR="$REPORT_DIR" \
  TNG_STORAGE_REPORT_INDEX="${TNG_STORAGE_REPORT_INDEX:-$REPORT_DIR/storage-certification-index.md}" \
  "$ROOT/scripts/storage_certification_index.sh"

{
  echo
  echo "## Boundaries"
  echo
  echo "- This script runs destructive-safe fixtures only; it creates and removes its own test files under the selected roots."
  echo "- Physical PV affinity remains evidence-only because ordinary LV path writes do not select a specific PV."
  echo "- `TNG_STORAGE_SKIP_URING=1` and `TNG_STORAGE_SKIP_MOVE_IMPORT=1` fail release reports unless `TNG_STORAGE_ALLOW_RELEASE_SKIP=1` is also set for an explicit dry run."
  echo
  if [[ "$overall" -eq 0 ]]; then
    echo "Overall status: PASS"
  else
    echo "Overall status: FAIL"
  fi
} >>"$OUT"

echo "$OUT"
exit "$overall"
