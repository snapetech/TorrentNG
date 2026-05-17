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

cd "$ROOT"
TNG_STORAGE_BENCH_DIR="$BENCH_DIR" \
TNG_STORAGE_BENCH_BLOCKS="${TNG_STORAGE_BENCH_BLOCKS:-16384}" \
TNG_STORAGE_BENCH_READS="${TNG_STORAGE_BENCH_READS:-100000}" \
cargo test -p rt-storage --release --test storage_real_device -- --ignored --nocapture
