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
  TNG_STORAGE_SYSCALLS           set to 1 to collect strace syscall counts
  TNG_STORAGE_LVM_EXTENTS        set to 1 to map a probe file through dm/LVM extents
  TNG_STORAGE_LVM_PROBE_MB       LVM extent probe file size (default: 256)
  TNG_STORAGE_LVM_PROBE_FILES    number of LVM probe files (default: 4)
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
  source="$(readlink -f "$source" 2>/dev/null || printf '%s\n' "$source")"
  local pkname
  pkname="$(lsblk -no PKNAME "$source" 2>/dev/null | head -1 || true)"
  if [[ -n "$pkname" ]]; then
    printf '/dev/%s\n' "$pkname"
  else
    printf '%s\n' "$source"
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

sudo_if_available() {
  if command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; then
    sudo "$@"
  else
    "$@"
  fi
}

append_lvm_extent_probe() {
  local target="$1"
  local source="$2"
  local root_block="$3"
  local probe_mb="${TNG_STORAGE_LVM_PROBE_MB:-256}"
  local probe_files="${TNG_STORAGE_LVM_PROBE_FILES:-4}"
  local clean_source="${source%%[*}"
  [[ "${TNG_STORAGE_LVM_EXTENTS:-0}" == "1" ]] || return 0
  [[ -b "$clean_source" && -b "$root_block" ]] || {
    echo
    echo "LVM/PV extent probe skipped: source or root block is not a block device."
    return 0
  }
  command -v filefrag >/dev/null 2>&1 || {
    echo
    echo "LVM/PV extent probe skipped: filefrag not found."
    return 0
  }
  command -v dmsetup >/dev/null 2>&1 || {
    echo
    echo "LVM/PV extent probe skipped: dmsetup not found."
    return 0
  }

  local probe_dir table extents devices
  probe_dir="$(mktemp -d "$target/tng-lvm-extent-probe-XXXXXX")"
  table="$tmpdir/lvm-table-$(basename "$target" | tr -c 'A-Za-z0-9_.-' '_')"
  extents="$tmpdir/lvm-extents-$(basename "$target" | tr -c 'A-Za-z0-9_.-' '_')"
  devices="$tmpdir/lvm-devices-$(basename "$target" | tr -c 'A-Za-z0-9_.-' '_')"

  local probes=()
  for ((i = 0; i < probe_files; i++)); do
    local probe="$probe_dir/probe-$i.bin"
    dd if=/dev/zero of="$probe" bs=1M count="$probe_mb" conv=fsync status=none
    probes+=("$probe")
  done
  if ! sudo_if_available dmsetup table "$clean_source" >"$table" 2>/dev/null; then
    echo
    echo "LVM/PV extent probe skipped: dmsetup table unavailable for $clean_source."
    rm -rf "$probe_dir"
    return 0
  fi
  if ! sudo_if_available filefrag -b512 -v "${probes[@]}" >"$extents" 2>/dev/null; then
    echo
    echo "LVM/PV extent probe skipped: filefrag unavailable for probe file."
    rm -rf "$probe_dir"
    return 0
  fi
  lsblk -rno MAJ:MIN,NAME,ROTA,TYPE >"$devices" 2>/dev/null || true

  echo
  echo "LVM/PV extent probe:"
  echo
  echo "| File | Extent | LV sector | Sectors | PV | PV sector | Rotational |"
  echo "| --- | ---: | ---: | ---: | --- | ---: | ---: |"
  awk -v table="$table" -v devices="$devices" '
    BEGIN {
      while ((getline line < devices) > 0) {
        split(line, d, " ");
        dev_name[d[1]] = d[2];
        dev_rota[d[1]] = d[3];
      }
      close(devices);
      while ((getline line < table) > 0) {
        split(line, t, " ");
        if (t[3] == "linear") {
          n++;
          lv_start[n] = t[1] + 0;
          lv_len[n] = t[2] + 0;
          pv_dev[n] = t[4];
          pv_start[n] = t[5] + 0;
        }
      }
      close(table);
    }
    /^File size of / {
      file = $4;
      sub(/^.*\//, "", file);
    }
    /^[[:space:]]*[0-9]+:/ {
      extent = $1;
      gsub(":", "", extent);
      phys = $4;
      gsub(":", "", phys);
      split(phys, range, /\.\./);
      lv_sector = range[1] + 0;
      sectors = $5;
      gsub(":", "", sectors);
      mapped = 0;
      for (i = 1; i <= n; i++) {
        if (lv_sector >= lv_start[i] && lv_sector < lv_start[i] + lv_len[i]) {
          pv_sector = pv_start[i] + (lv_sector - lv_start[i]);
          dev = pv_dev[i];
          name = (dev in dev_name) ? "/dev/" dev_name[dev] : dev;
          rota = (dev in dev_rota) ? dev_rota[dev] : "unknown";
          printf "| %s | %s | %s | %s | %s | %s | %s |\n", file, extent, lv_sector, sectors, name, pv_sector, rota;
          mapped = 1;
          break;
        }
      }
      if (!mapped) {
        printf "| %s | %s | %s | %s | unmapped |  | unknown |\n", file, extent, lv_sector, sectors;
      }
    }
  ' "$extents" | sed -n '1,32p'
  rm -rf "$probe_dir"
}

append_summary() {
  local log="$1"
  {
    grep -E 'tng_storage_(backend|bench_path|file_pool|readahead|shuffled_baseline|elevator)' "$log" || true
    grep -E 'tng_storage_syscalls' "$log" || true
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
  echo "- Syscall counts: ${TNG_STORAGE_SYSCALLS:-0}"
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
    append_lvm_extent_probe "$target" "$source" "$root_block"
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
    TNG_STORAGE_SYSCALLS="${TNG_STORAGE_SYSCALLS:-0}" \
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
