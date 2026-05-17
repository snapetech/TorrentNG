#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="$ROOT/certification/reports"
OUT="${1:-$REPORT_DIR/memory-roadmap-certification-$(date -u +%Y%m%dT%H%M%SZ).md}"

mkdir -p "$(dirname "$OUT")"
overall=0

latest_report() {
  local pattern="$1"
  find "$REPORT_DIR" -maxdepth 1 -type f -name "$pattern" -printf '%T@ %p\n' 2>/dev/null |
    sort -nr |
    awk 'NR == 1 { print $2 }'
}

contains() {
  local file="$1"
  local pattern="$2"
  [[ -n "$file" && -f "$file" ]] && grep -Eq "$pattern" "$file"
}

row() {
  local name="$1"
  local result="$2"
  local evidence="$3"
  if [[ "$result" == "FAIL" ]]; then
    overall=1
  fi
  printf '| %s | %s | %s |\n' "$name" "$result" "$evidence" >>"$OUT"
}

report_link() {
  local file="$1"
  if [[ -n "$file" ]]; then
    printf '%s' "${file#$ROOT/}"
  else
    printf 'missing'
  fi
}

local_release="$(latest_report 'local-release-*.md')"
move_import="$(latest_report 'storage-move-import-*.md')"
move_import_realroot="$(latest_report 'storage-move-import-realroot-*.md')"
storage_hdd="$(latest_report 'storage-hardware-kspls0-lvm-hdd-*.md')"
storage_pvmap="$(latest_report 'storage-hardware-kspls0-lvm-pvmap-*.md')"
storage_uring="$(latest_report 'storage-uring-graduation-*.md')"

{
  echo "# TorrentNG Memory Roadmap Certification"
  echo
  echo "- Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo
  echo "| Roadmap item | Status | Evidence |"
  echo "| --- | --- | --- |"
} >"$OUT"

if contains "$local_release" 'idle_memory_100k_keeps_fixed_rss_task_fd_budget.*ok'; then
  row "10k/100k idle RSS/task/fd proxy" PASS "$(report_link "$local_release")"
else
  row "10k/100k idle RSS/task/fd proxy" FAIL "$(report_link "$local_release")"
fi

if contains "$local_release" 'hot_seeding_1k_memory_attribution_stays_under_cap.*ok'; then
  row "1k hot seeding memory-cap proxy" PASS "$(report_link "$local_release")"
else
  row "1k hot seeding memory-cap proxy" FAIL "$(report_link "$local_release")"
fi

if contains "$local_release" 'storage_hash_pool_does_not_block_peer_read_path.*ok' &&
  contains "$local_release" 'full_mount_queue_fails_closed.*ok'; then
  row "slow-disk/fast-peer backpressure proxy" PASS "$(report_link "$local_release")"
else
  row "slow-disk/fast-peer backpressure proxy" FAIL "$(report_link "$local_release")"
fi

if contains "$local_release" 'queued_disk_governor_denies_before_enqueue.*ok' &&
  contains "$local_release" 'queued_disk_bytes_track_active_blocking_job_payload.*ok'; then
  row "hard queued-disk memory leases" PASS "$(report_link "$local_release")"
else
  row "hard queued-disk memory leases" FAIL "$(report_link "$local_release")"
fi

if contains "$local_release" 'schedulers_on_same_device_share_global_queue.*ok'; then
  row "process-level per-device queue registry" PASS "$(report_link "$local_release")"
else
  row "process-level per-device queue registry" FAIL "$(report_link "$local_release")"
fi

if contains "$move_import" 'Overall status: PASS|test result: ok' &&
  contains "$move_import" 'symlink'; then
  row "move/import/delete executor safety" PASS "$(report_link "$move_import")"
else
  row "move/import/delete executor safety" FAIL "$(report_link "$move_import")"
fi

if contains "$move_import_realroot" 'tng_storage_move_import .*root_confined=1'; then
  row "real-root move/import fixture evidence" PASS "$(report_link "$move_import_realroot")"
else
  row "real-root move/import fixture evidence" WARN "$(report_link "$move_import_realroot")"
fi

if contains "$storage_hdd" 'Overall status: PASS' &&
  contains "$storage_hdd" 'TorrentNG storage elevator wall-clock ratio: [5-9][0-9]*\.|TorrentNG storage elevator wall-clock ratio: [5-9]\.'; then
  row "HDD 5x elevator release evidence" PASS "$(report_link "$storage_hdd")"
else
  row "HDD 5x elevator release evidence" WARN "$(report_link "$storage_hdd")"
fi

if contains "$storage_pvmap" '/dev/sd.*\|.*\| 1 \|'; then
  row "sampled LVM physical-PV placement evidence" PASS "$(report_link "$storage_pvmap")"
else
  row "sampled LVM physical-PV placement evidence" WARN "$(report_link "$storage_pvmap")"
fi

if contains "$storage_uring" 'Overall status: PASS' &&
  contains "$storage_uring" 'Selected: uring'; then
  row "io_uring real-device graduation probe" PASS "$(report_link "$storage_uring")"
else
  row "io_uring real-device graduation probe" WARN "$(report_link "$storage_uring")"
fi

{
  echo
  echo "## Boundaries"
  echo
  echo "- Deterministic LVM physical-drive placement remains a non-claim unless a lower-level PV-targeted path is added."
  echo "- io_uring remains explicit opt-in until graduation reports meet selected-backend, fixed-buffer strategy, registered-file, and throughput thresholds on target hardware."
  echo "- Multi-TB move/import certification remains host/run evidence; use the real-root fixture knobs to scale the report on the target storage root."
} >>"$OUT"

echo "memory roadmap report: $OUT"
exit "$overall"
