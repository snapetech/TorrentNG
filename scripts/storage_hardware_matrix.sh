#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${TNG_STORAGE_MATRIX_REPORT:-$ROOT/certification/reports/storage-hardware-$(date -u +%Y%m%dT%H%M%SZ).md}"

usage() {
  cat >&2 <<'USAGE'
usage: scripts/storage_hardware_matrix.sh /mount/or/path [...]

Runs the real-device Storage NG probes once per target path and writes a
markdown report under certification/reports/.

Environment:
  TNG_STORAGE_BENCH_BLOCKS       blocks per benchmark file (default: 4096)
  TNG_STORAGE_BENCH_READS        repeated hot-file reads (default: 10000)
  TNG_STORAGE_REQUIRE_HDD_5X     require >=5x elevator wall-clock on HDD paths
  TNG_STORAGE_MATRIX_REPORT      report path override
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

mkdir -p "$(dirname "$OUT")"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

source_for_path() {
  findmnt -n -T "$1" -o SOURCE 2>/dev/null || true
}

fstype_for_path() {
  findmnt -n -T "$1" -o FSTYPE 2>/dev/null || true
}

root_block_for_source() {
  local source="$1"
  source="${source%%[*}"
  [[ -b "$source" ]] || return 0
  local pkname
  pkname="$(lsblk -no PKNAME "$source" 2>/dev/null | head -1 || true)"
  if [[ -n "$pkname" ]]; then
    printf '/dev/%s\n' "$pkname"
  else
    lsblk -no NAME "$source" 2>/dev/null | head -1 | sed 's#^#/dev/#'
  fi
}

rotational_for_block() {
  local block="$1"
  [[ -n "$block" && -b "$block" ]] || {
    printf 'unknown\n'
    return
  }
  lsblk -dnro ROTA "$block" 2>/dev/null | head -1 || printf 'unknown\n'
}

profile_for_rota() {
  case "$1" in
    1) printf 'HDD\n' ;;
    0) printf 'SSD/NVMe\n' ;;
    *) printf 'unknown/network\n' ;;
  esac
}

append_summary() {
  local log="$1"
  {
    grep -E 'tng_storage_(bench_path|file_pool|readahead|shuffled_baseline|elevator)' "$log" || true
    grep -E 'TorrentNG storage elevator wall-clock ratio|expected >=5x' "$log" || true
  } | sed 's/^/    /'
}

{
  echo "# TorrentNG Storage Hardware Matrix"
  echo
  echo "- Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "- Blocks: ${TNG_STORAGE_BENCH_BLOCKS:-4096}"
  echo "- Hot reads: ${TNG_STORAGE_BENCH_READS:-10000}"
  echo
} >"$OUT"

overall=0

for target in "$@"; do
  mkdir -p "$target"
  source="$(source_for_path "$target")"
  fstype="$(fstype_for_path "$target")"
  root_block="$(root_block_for_source "$source")"
  rota="$(rotational_for_block "$root_block")"
  profile="$(profile_for_rota "$rota")"
  log="$tmpdir/$(basename "$target" | tr -c 'A-Za-z0-9_.-' '_').log"

  {
    echo "## $target"
    echo
    echo "| Field | Value |"
    echo "| --- | --- |"
    echo "| mount source | ${source:-unknown} |"
    echo "| filesystem | ${fstype:-unknown} |"
    echo "| root block | ${root_block:-unknown} |"
    echo "| rotational | ${rota:-unknown} |"
    echo "| inferred profile | $profile |"
    echo
  } >>"$OUT"

  echo "== TorrentNG storage hardware matrix: $target ($profile) =="
  if [[ "$rota" == "1" && "${TNG_STORAGE_REQUIRE_HDD_5X:-0}" == "1" ]]; then
    require_5x=1
  else
    require_5x=0
  fi

  if TNG_STORAGE_BENCH_BLOCKS="${TNG_STORAGE_BENCH_BLOCKS:-4096}" \
    TNG_STORAGE_BENCH_READS="${TNG_STORAGE_BENCH_READS:-10000}" \
    TNG_STORAGE_REQUIRE_5X="$require_5x" \
    "$ROOT/scripts/storage_real_device_benchmark.sh" "$target" 2>&1 | tee "$log"; then
    echo "- Result: PASS" >>"$OUT"
  else
    echo "- Result: FAIL" >>"$OUT"
    overall=1
  fi

  echo >>"$OUT"
  echo "Summary:" >>"$OUT"
  echo >>"$OUT"
  append_summary "$log" >>"$OUT"
  echo >>"$OUT"
done

{
  echo "## Gate"
  echo
  if [[ "$overall" -eq 0 ]]; then
    echo "PASS"
    echo
    echo "Overall status: PASS"
  else
    echo "FAIL"
    echo
    echo "Overall status: FAIL"
  fi
} >>"$OUT"

echo "storage hardware report: $OUT"
exit "$overall"
