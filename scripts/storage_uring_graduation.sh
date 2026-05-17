#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BENCH_DIR="${1:-${TNG_STORAGE_BENCH_DIR:-}}"
OUT="${TNG_STORAGE_URING_REPORT:-$ROOT/certification/reports/storage-uring-graduation-$(date -u +%Y%m%dT%H%M%SZ).md}"

usage() {
  cat >&2 <<'USAGE'
usage: scripts/storage_uring_graduation.sh /path/on/storage

Runs a real-device pread vs io_uring backend stream benchmark and writes a
markdown report under certification/reports/.

Environment:
  TNG_STORAGE_BACKEND_STREAM_BLOCKS      stream blocks (default: 1024)
  TNG_STORAGE_BACKEND_STREAM_BLOCK_LEN   block bytes (default: 262144)
  TNG_STORAGE_URING_REQUIRE_SELECTED     require selected=uring for the uring run
  TNG_STORAGE_URING_REQUIRE_FIXED        require fixed_buffers=true
  TNG_STORAGE_URING_REQUIRE_FILES        require registered_files=true
  TNG_STORAGE_URING_MIN_READ_RATIO       require uring read MiB/s >= ratio * pread
  TNG_STORAGE_URING_MIN_WRITE_RATIO      require uring write MiB/s >= ratio * pread
  TNG_STORAGE_URING_REPORT               report path override
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ -z "$BENCH_DIR" ]]; then
  usage
  exit 2
fi

mkdir -p "$BENCH_DIR" "$(dirname "$OUT")"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

run_backend() {
  local backend="$1"
  local log="$tmpdir/$backend.log"
  TNG_STORAGE_BACKEND="$backend" \
    TNG_STORAGE_BENCH_DIR="$BENCH_DIR" \
    cargo test -p rt-storage --release --test storage_real_device \
    backend_stream_roundtrip_reports_throughput -- --ignored --nocapture --test-threads=1 \
    >"$log" 2>&1
  cat "$log"
}

field_from_log() {
  local field="$1"
  local log="$2"
  sed -n "s/.*tng_storage_backend_stream .*${field}=\\([^ ]*\\).*/\\1/p" "$log" | tail -1
}

overall=0
cd "$ROOT"

{
  echo "# TorrentNG io_uring Graduation Report"
  echo
  echo "- Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "- Target: $BENCH_DIR"
  echo "- Blocks: ${TNG_STORAGE_BACKEND_STREAM_BLOCKS:-1024}"
  echo "- Block length: ${TNG_STORAGE_BACKEND_STREAM_BLOCK_LEN:-262144}"
  echo
} >"$OUT"

for backend in pread uring; do
  log="$tmpdir/$backend.log"
  echo "== TorrentNG backend stream: $backend =="
  if run_backend "$backend" | tee "$log"; then
    result="PASS"
  else
    result="FAIL"
    overall=1
  fi
  {
    echo "## $backend"
    echo
    echo "- Result: $result"
    echo "- Selected: $(field_from_log selected "$log")"
    echo "- Read MiB/s: $(field_from_log read_mib_s "$log")"
    echo "- Write MiB/s: $(field_from_log write_mib_s "$log")"
    echo "- Fixed buffers: $(field_from_log fixed_buffers "$log")"
    echo "- Registered files: $(field_from_log registered_files "$log")"
    echo
    echo '```text'
    cat "$log"
    echo '```'
    echo
  } >>"$OUT"
done

pread_read="$(field_from_log read_mib_s "$tmpdir/pread.log")"
pread_write="$(field_from_log write_mib_s "$tmpdir/pread.log")"
uring_read="$(field_from_log read_mib_s "$tmpdir/uring.log")"
uring_write="$(field_from_log write_mib_s "$tmpdir/uring.log")"
uring_selected="$(field_from_log selected "$tmpdir/uring.log")"
uring_fixed="$(field_from_log fixed_buffers "$tmpdir/uring.log")"
uring_files="$(field_from_log registered_files "$tmpdir/uring.log")"

{
  echo "## Graduation Gates"
  echo
  echo "| Gate | Result |"
  echo "| --- | --- |"
} >>"$OUT"

gate() {
  local name="$1"
  shift
  if "$@"; then
    echo "| $name | PASS |" >>"$OUT"
  else
    echo "| $name | FAIL |" >>"$OUT"
    overall=1
  fi
}

if [[ "${TNG_STORAGE_URING_REQUIRE_SELECTED:-0}" == "1" ]]; then
  gate "uring selected" test "$uring_selected" = "uring"
else
  echo "| uring selected | INFO: $uring_selected |" >>"$OUT"
fi

if [[ "${TNG_STORAGE_URING_REQUIRE_FIXED:-0}" == "1" ]]; then
  gate "fixed buffers" test "$uring_fixed" = "true"
else
  echo "| fixed buffers | INFO: $uring_fixed |" >>"$OUT"
fi

if [[ "${TNG_STORAGE_URING_REQUIRE_FILES:-0}" == "1" ]]; then
  gate "registered files" test "$uring_files" = "true"
else
  echo "| registered files | INFO: $uring_files |" >>"$OUT"
fi

if [[ -n "${TNG_STORAGE_URING_MIN_READ_RATIO:-}" && -n "$pread_read" && -n "$uring_read" ]]; then
  gate "read throughput ratio >= ${TNG_STORAGE_URING_MIN_READ_RATIO}" \
    awk -v p="$pread_read" -v u="$uring_read" -v r="$TNG_STORAGE_URING_MIN_READ_RATIO" \
    'BEGIN { exit (u >= p * r) ? 0 : 1 }'
else
  echo "| read throughput ratio | INFO: not required |" >>"$OUT"
fi

if [[ -n "${TNG_STORAGE_URING_MIN_WRITE_RATIO:-}" && -n "$pread_write" && -n "$uring_write" ]]; then
  gate "write throughput ratio >= ${TNG_STORAGE_URING_MIN_WRITE_RATIO}" \
    awk -v p="$pread_write" -v u="$uring_write" -v r="$TNG_STORAGE_URING_MIN_WRITE_RATIO" \
    'BEGIN { exit (u >= p * r) ? 0 : 1 }'
else
  echo "| write throughput ratio | INFO: not required |" >>"$OUT"
fi

{
  echo
  if [[ "$overall" -eq 0 ]]; then
    echo "Overall status: PASS"
  else
    echo "Overall status: FAIL"
  fi
} >>"$OUT"

echo "storage uring graduation report: $OUT"
exit "$overall"
