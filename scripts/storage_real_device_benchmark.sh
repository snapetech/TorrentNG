#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BENCH_DIR="${1:-${TNG_STORAGE_BENCH_DIR:-}}"

if [[ -z "$BENCH_DIR" ]]; then
  echo "usage: $0 /path/on/storage" >&2
  echo "or set TNG_STORAGE_BENCH_DIR=/path/on/storage" >&2
  exit 2
fi

mkdir -p "$BENCH_DIR"

source_dev="$(findmnt -n -T "$BENCH_DIR" -o SOURCE 2>/dev/null || true)"
dev="${source_dev%%[*}"
rota="unknown"
if [[ -n "$dev" ]]; then
  pkname="$(lsblk -no PKNAME "$dev" 2>/dev/null | head -1 || true)"
  if [[ -n "$pkname" ]]; then
    rota="$(lsblk -dnro ROTA "/dev/$pkname" 2>/dev/null | head -1 || true)"
  else
    rota="$(lsblk -no ROTA "$dev" 2>/dev/null | head -1 || true)"
  fi
fi

echo "TorrentNG storage benchmark dir: $BENCH_DIR"
echo "Backing source: ${source_dev:-unknown}"
echo "Rotational flag: ${rota:-unknown}"
echo "Syscall summary: ${TNG_STORAGE_SYSCALLS:-0}"

cd "$ROOT"

export TNG_STORAGE_BENCH_DIR="$BENCH_DIR"
export TNG_STORAGE_BENCH_BLOCKS="${TNG_STORAGE_BENCH_BLOCKS:-16384}"
export TNG_STORAGE_BENCH_READS="${TNG_STORAGE_BENCH_READS:-100000}"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

run_case() {
  local name="$1"
  local log="$tmpdir/$name.log"
  local trace="$tmpdir/$name.strace"
  echo
  echo "==> $name"
  if [[ "${TNG_STORAGE_SYSCALLS:-0}" == "1" && -x "$(command -v strace 2>/dev/null || true)" ]]; then
    strace -f -qq \
      -e trace=open,openat,close,read,write,pread64,pwrite64,fsync,fdatasync,fallocate,statx \
      -o "$trace" \
      cargo test -p rt-storage --release --test storage_real_device "$name" -- --ignored --nocapture --test-threads=1 \
      2>&1 | tee "$log"
    summarize_syscalls "$name" "$trace"
  else
    if [[ "${TNG_STORAGE_SYSCALLS:-0}" == "1" ]]; then
      echo "tng_storage_syscalls case=$name unavailable=strace-not-found"
    fi
    cargo test -p rt-storage --release --test storage_real_device "$name" -- --ignored --nocapture --test-threads=1 \
      2>&1 | tee "$log"
  fi
}

run_backend_case() {
  local backend="$1"
  local name="backend_selection_roundtrip_reports_capabilities"
  local log="$tmpdir/backend-$backend.log"
  echo
  echo "==> $name backend=$backend"
  TNG_STORAGE_BACKEND="$backend" \
    cargo test -p rt-storage --release --test storage_real_device "$name" -- --ignored --nocapture --test-threads=1 \
    2>&1 | tee "$log"
}

summarize_syscalls() {
  local name="$1"
  local trace="$2"
  [[ -s "$trace" ]] || {
    echo "tng_storage_syscalls case=$name unavailable=empty-trace"
    return
  }
  awk -v case_name="$name" '
    {
      line = $0
      sub(/^[0-9]+[[:space:]]+/, "", line)
      if (line ~ /^[a-zA-Z0-9_]+\(/) {
        syscall = line
        sub(/\(.*/, "", syscall)
        calls[syscall]++
        if (line ~ / = -1 /) {
          errors[syscall]++
        }
      }
    }
    END {
      for (syscall in calls) {
        printf "tng_storage_syscalls case=%s syscall=%s calls=%d errors=%d\n",
          case_name, syscall, calls[syscall], errors[syscall] + 0
      }
    }
  ' "$trace" | sort
}

elapsed_ms() {
  local key="$1"
  local log="$2"
  sed -n "s/.*${key}.*elapsed_ms=\\([0-9][0-9]*\\).*/\\1/p" "$log" | tail -1
}

run_backend_case pread
run_backend_case uring
run_case peer_read_readahead_reduces_backend_reads_on_adjacent_blocks
run_case repeated_reads_reuse_one_open_file_handle
run_case recheck_range_reports_runtime_progress
run_case shuffled_peer_read_baseline_reports_current_scheduler_throughput
run_case hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks

baseline_ms="$(elapsed_ms tng_storage_shuffled_baseline "$tmpdir/shuffled_peer_read_baseline_reports_current_scheduler_throughput.log")"
elevator_ms="$(elapsed_ms tng_storage_elevator "$tmpdir/hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks.log")"

if [[ -n "$baseline_ms" && -n "$elevator_ms" && "$baseline_ms" != "0" && "$elevator_ms" != "0" ]]; then
  ratio="$(awk -v b="$baseline_ms" -v e="$elevator_ms" 'BEGIN { printf "%.2f", b / e }')"
  echo
  echo "TorrentNG storage elevator wall-clock ratio: ${ratio}x baseline/elevator (${baseline_ms}ms/${elevator_ms}ms)"
  if [[ "${TNG_STORAGE_REQUIRE_5X:-0}" == "1" ]]; then
    awk -v r="$ratio" 'BEGIN { exit (r >= 5.0) ? 0 : 1 }' || {
      echo "expected >=5x wall-clock speedup; got ${ratio}x" >&2
      exit 1
    }
  fi
else
  echo
  echo "TorrentNG storage elevator wall-clock ratio unavailable; likely skipped on non-HDD topology"
fi
