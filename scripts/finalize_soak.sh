#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT="${1:-}"
OUT="${2:-$ROOT/certification/reports/soak-final-$(date -u +%Y%m%dT%H%M%SZ).md}"
MIN_SAMPLES="${SOAK_MIN_SAMPLES:-1200}"
MIN_TORRENTS="${SOAK_MIN_TORRENTS:-15000}"
MAX_RSS_MB="${SOAK_MAX_RSS_MB:-500}"
MAX_FDS="${SOAK_MAX_FDS:-4096}"
MAX_THREADS="${SOAK_MAX_THREADS:-512}"
MIN_DISK_FREE_MB="${SOAK_MIN_DISK_FREE_MB:-100}"
RESTORE_NORMAL="${RESTORE_NORMAL:-0}"
ALLOW_INCOMPLETE="${SOAK_ALLOW_INCOMPLETE:-0}"

if [[ -z "$REPORT" ]]; then
  REPORT="$(find "$ROOT/certification/reports" -maxdepth 1 -type f -name 'soak-24h-*.md' -printf '%T@ %p\n' 2>/dev/null | sort -nr | awk 'NR==1 {print $2}')"
fi

mkdir -p "$(dirname "$OUT")"
status="PASS"

mark() {
  local check="$1"
  local result="$2"
  local detail="$3"
  printf '| %s | %s | %s |\n' "$check" "$result" "$detail" >> "$OUT"
  if [[ "$result" == "FAIL" ]]; then
    status="FAIL"
  fi
}

{
  echo "# TorrentNG Soak Finalization"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Source report: ${REPORT:-missing}"
  echo "- Minimum samples: $MIN_SAMPLES"
  echo "- Minimum torrents: $MIN_TORRENTS"
  echo "- Max RSS MB: $MAX_RSS_MB"
  echo "- Max file descriptors: $MAX_FDS"
  echo "- Max threads: $MAX_THREADS"
  echo "- Minimum disk free MB: $MIN_DISK_FREE_MB"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

if [[ -z "$REPORT" || ! -f "$REPORT" ]]; then
  mark "source report" "FAIL" "missing soak report"
else
  mark "source report" "PASS" "$REPORT"

  sample_count="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {count++} END {print count+0}' "$REPORT")"
  if (( sample_count >= MIN_SAMPLES )); then
    mark "sample count" "PASS" "$sample_count >= $MIN_SAMPLES"
  else
    mark "sample count" "FAIL" "$sample_count < $MIN_SAMPLES"
  fi

  min_torrents="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/ /, "", $4); if (min == "" || $4 < min) min=$4} END {print min == "" ? 0 : min}' "$REPORT")"
  if (( min_torrents >= MIN_TORRENTS )); then
    mark "torrent floor" "PASS" "$min_torrents >= $MIN_TORRENTS"
  else
    mark "torrent floor" "FAIL" "$min_torrents < $MIN_TORRENTS"
  fi

  max_rss="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/ /, "", $5); if ($5+0 > max) max=$5+0} END {printf "%.1f", max}' "$REPORT")"
  if awk -v rss="$max_rss" -v limit="$MAX_RSS_MB" 'BEGIN {exit !(rss <= limit)}'; then
    mark "memory ceiling" "PASS" "${max_rss}MB <= ${MAX_RSS_MB}MB"
  else
    mark "memory ceiling" "FAIL" "${max_rss}MB > ${MAX_RSS_MB}MB"
  fi

  bad_health="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/^[[:space:]]+|[[:space:]]+$/, "", $3); if ($3 != "200") bad++} END {print bad+0}' "$REPORT")"
  if (( bad_health == 0 )); then
    mark "health samples" "PASS" "all HTTP 200"
  else
    mark "health samples" "FAIL" "$bad_health non-200 samples"
  fi

  bad_sync="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/^[[:space:]]+|[[:space:]]+$/, "", $6); if ($6 != "200") bad++} END {print bad+0}' "$REPORT")"
  if (( bad_sync == 0 )); then
    mark "sync samples" "PASS" "all HTTP 200"
  else
    mark "sync samples" "FAIL" "$bad_sync non-200 samples"
  fi

  if grep -q '^| UTC | Health | Torrents | RSS MB | sync/maindata HTTP | FDs | Threads | Disk free MB | Metrics HTTP | DB/Cache | Storage |' "$REPORT"; then
    max_fds="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $7); if ($7 + 0 > max) max = $7 + 0} END {print max + 0}' "$REPORT")"
    max_threads="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $8); if ($8 + 0 > max) max = $8 + 0} END {print max + 0}' "$REPORT")"
    min_disk="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $9); if (seen == 0 || ($9 + 0) < min) min = $9 + 0; seen = 1} END {print seen ? min : 0}' "$REPORT")"
    bad_metrics="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $10); if ($10 != "200") bad++} END {print bad + 0}' "$REPORT")"
    bad_components="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {for (i = 11; i <= 12; i++) {gsub(/[[:space:]]/, "", $i); if ($i == "unhealthy" || $i == "unknown") bad++}} END {print bad + 0}' "$REPORT")"
    if (( max_fds <= MAX_FDS )); then
      mark "file-descriptor ceiling" "PASS" "${max_fds} <= ${MAX_FDS}"
    else
      mark "file-descriptor ceiling" "FAIL" "${max_fds} > ${MAX_FDS}"
    fi
    if (( max_threads <= MAX_THREADS )); then
      mark "thread ceiling" "PASS" "${max_threads} <= ${MAX_THREADS}"
    else
      mark "thread ceiling" "FAIL" "${max_threads} > ${MAX_THREADS}"
    fi
    if (( min_disk >= MIN_DISK_FREE_MB )); then
      mark "disk-free floor" "PASS" "${min_disk}MB >= ${MIN_DISK_FREE_MB}MB"
    else
      mark "disk-free floor" "FAIL" "${min_disk}MB < ${MIN_DISK_FREE_MB}MB"
    fi
    if (( bad_metrics == 0 )); then
      mark "metrics samples" "PASS" "all HTTP 200"
    else
      mark "metrics samples" "FAIL" "$bad_metrics non-200 samples"
    fi
    if (( bad_components == 0 )); then
      mark "dependency health fields" "PASS" "no unhealthy or unknown fields"
    else
      mark "dependency health fields" "FAIL" "$bad_components unhealthy/unknown fields"
    fi
  else
    mark "extended process telemetry" "INFO" "legacy report has no FD/thread/disk/metrics columns"
  fi

  if grep -q '^Overall status: PASS' "$REPORT"; then
    mark "source completion" "PASS" "source report completed PASS"
  elif [[ "$ALLOW_INCOMPLETE" == "1" ]]; then
    mark "source completion" "INFO" "source report still running"
  else
    mark "source completion" "FAIL" "source report has not completed PASS"
  fi
fi

if [[ "$RESTORE_NORMAL" == "1" ]]; then
  if "$ROOT/scripts/restore_certification_normal.sh" >/tmp/tng-restore-normal.log 2>&1; then
    mark "restore normal sync" "PASS" "TNG_SYNC_INTERVAL_SECS=2"
  else
    mark "restore normal sync" "FAIL" "$(tr '\n' ' ' </tmp/tng-restore-normal.log)"
  fi
else
  mark "restore normal sync" "INFO" "set RESTORE_NORMAL=1 to restore certification service"
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
