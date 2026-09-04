#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT="${1:-}"
OUT="${2:-}"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
MIN_TORRENTS="${SOAK_MIN_TORRENTS:-15000}"
MAX_RSS_MB="${SOAK_MAX_RSS_MB:-500}"
EXPECTED_SECONDS="${SOAK_DURATION_SECONDS:-86400}"
MAX_FDS="${SOAK_MAX_FDS:-4096}"
MAX_THREADS="${SOAK_MAX_THREADS:-512}"
MIN_DISK_FREE_MB="${SOAK_MIN_DISK_FREE_MB:-100}"

if [[ -z "$REPORT" ]]; then
  REPORT="$(find "$REPORT_DIR" -maxdepth 1 -type f -name 'soak-24h-*.md' -printf '%T@ %p\n' 2>/dev/null | sort -nr | awk 'NR==1 {print $2}')"
fi

if [[ -z "$REPORT" || ! -f "$REPORT" ]]; then
  echo "missing soak report" >&2
  exit 1
fi

if [[ -n "$OUT" ]]; then
  mkdir -p "$(dirname "$OUT")"
  exec > "$OUT"
fi

samples="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {count++} END {print count+0}' "$REPORT")"
first_ts="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/^[[:space:]]+|[[:space:]]+$/, "", $2); print $2; exit}' "$REPORT")"
last_line="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {line=$0} END {print line}' "$REPORT")"
last_ts="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/^[[:space:]]+|[[:space:]]+$/, "", $2); ts=$2} END {print ts}' "$REPORT")"
min_torrents="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/ /, "", $4); if (min == "" || $4 < min) min=$4} END {print min == "" ? 0 : min}' "$REPORT")"
max_rss="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/ /, "", $5); if ($5+0 > max) max=$5+0} END {printf "%.1f", max}' "$REPORT")"
bad_health="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/^[[:space:]]+|[[:space:]]+$/, "", $3); if ($3 != "200") bad++} END {print bad+0}' "$REPORT")"
bad_sync="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/^[[:space:]]+|[[:space:]]+$/, "", $6); if ($6 != "200") bad++} END {print bad+0}' "$REPORT")"
extended=0
if grep -q '^| UTC | Health | Torrents | RSS MB | sync/maindata HTTP | FDs | Threads | Disk free MB | Metrics HTTP | DB/Cache | Storage |' "$REPORT"; then
  extended=1
fi
max_fds="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $7); if ($7 + 0 > max) max = $7 + 0} END {print max + 0}' "$REPORT")"
max_threads="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $8); if ($8 + 0 > max) max = $8 + 0} END {print max + 0}' "$REPORT")"
min_disk="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $9); if (seen == 0 || ($9 + 0) < min) min = $9 + 0; seen = 1} END {print seen ? min : 0}' "$REPORT")"
bad_metrics="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $10); if ($10 != "200") bad++} END {print bad + 0}' "$REPORT")"
bad_components="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {for (i = 11; i <= 12; i++) {gsub(/[[:space:]]/, "", $i); if ($i == "unhealthy" || $i == "unknown") bad++}} END {print bad + 0}' "$REPORT")"
active="$(pgrep -af '[s]oak_certification.sh' | grep -F "$(basename "$REPORT")" || true)"

elapsed="unknown"
remaining="unknown"
if [[ -n "$first_ts" && -n "$last_ts" ]] && command -v date >/dev/null 2>&1; then
  first_epoch="$(date -u -d "$first_ts" +%s 2>/dev/null || true)"
  last_epoch="$(date -u -d "$last_ts" +%s 2>/dev/null || true)"
  if [[ "$first_epoch" =~ ^[0-9]+$ && "$last_epoch" =~ ^[0-9]+$ ]]; then
    elapsed="$((last_epoch - first_epoch))"
    left=$((EXPECTED_SECONDS - elapsed))
    (( left < 0 )) && left=0
    remaining="$left"
  fi
fi

status="PASS"
(( min_torrents >= MIN_TORRENTS )) || status="FAIL"
awk -v rss="$max_rss" -v limit="$MAX_RSS_MB" 'BEGIN {exit !(rss <= limit)}' || status="FAIL"
(( bad_health == 0 )) || status="FAIL"
(( bad_sync == 0 )) || status="FAIL"
if (( extended )); then
  (( max_fds <= MAX_FDS )) || status="FAIL"
  (( max_threads <= MAX_THREADS )) || status="FAIL"
  (( min_disk >= MIN_DISK_FREE_MB )) || status="FAIL"
  (( bad_metrics == 0 )) || status="FAIL"
  (( bad_components == 0 )) || status="FAIL"
fi

echo "# TorrentNG Soak Status"
echo
echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "- Report: $REPORT"
echo "- Active process: ${active:-none}"
echo "- First sample: ${first_ts:-none}"
echo "- Last sample: ${last_ts:-none}"
echo "- Samples: $samples"
echo "- Elapsed seconds: $elapsed"
echo "- Remaining seconds target: $remaining"
echo
echo "| Check | Status | Detail |"
echo "|---|---|---|"
echo "| Torrent floor | $([[ "$min_torrents" -ge "$MIN_TORRENTS" ]] && echo PASS || echo FAIL) | min=$min_torrents target>=$MIN_TORRENTS |"
echo "| Memory ceiling | $(awk -v rss="$max_rss" -v limit="$MAX_RSS_MB" 'BEGIN {print (rss <= limit) ? "PASS" : "FAIL"}') | max=${max_rss}MB target<=${MAX_RSS_MB}MB |"
echo "| Health samples | $([[ "$bad_health" -eq 0 ]] && echo PASS || echo FAIL) | bad=$bad_health |"
echo "| Sync samples | $([[ "$bad_sync" -eq 0 ]] && echo PASS || echo FAIL) | bad=$bad_sync |"
if (( extended )); then
  echo "| File-descriptor ceiling | $([[ "$max_fds" -le "$MAX_FDS" ]] && echo PASS || echo FAIL) | max=$max_fds target<=$MAX_FDS |"
  echo "| Thread ceiling | $([[ "$max_threads" -le "$MAX_THREADS" ]] && echo PASS || echo FAIL) | max=$max_threads target<=$MAX_THREADS |"
  echo "| Disk-free floor | $([[ "$min_disk" -ge "$MIN_DISK_FREE_MB" ]] && echo PASS || echo FAIL) | min=${min_disk}MB target>=${MIN_DISK_FREE_MB}MB |"
  echo "| Metrics samples | $([[ "$bad_metrics" -eq 0 ]] && echo PASS || echo FAIL) | bad=$bad_metrics |"
  echo "| Dependency health fields | $([[ "$bad_components" -eq 0 ]] && echo PASS || echo FAIL) | bad=$bad_components |"
else
  echo "| Extended process telemetry | INFO | legacy report has no FD/thread/disk/metrics columns |"
fi
echo "| Latest sample | INFO | ${last_line//|/\\|} |"
echo
echo "Overall status: $status"

[[ "$status" == "PASS" ]]
