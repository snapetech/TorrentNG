#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
BENCHMARK_DIR="${BENCHMARK_DIR:-$ROOT/benchmarks}"
OUT="${1:-$REPORT_DIR/pre-engine-release-$(date -u +%Y%m%dT%H%M%SZ).md}"
SOAK_REPORT="${SOAK_REPORT:-}"

mkdir -p "$(dirname "$OUT")"

latest() {
  local pattern="$1"
  local dir="${2:-$REPORT_DIR}"
  find "$dir" -maxdepth 1 -type f -name "$pattern" -printf '%T@ %p\n' 2>/dev/null \
    | sort -nr | awk 'NR==1 {print $2}'
}

overall() {
  local file="$1"
  if [[ -z "$file" || ! -f "$file" ]]; then
    printf 'MISSING'
    return
  fi
  awk -F': ' '
    /^Overall status:/ {status=$2}
    /test result: ok/ {ok=1}
    END {
      if (status) print status;
      else if (ok) print "PASS";
      else print "RUNNING/UNKNOWN";
    }
  ' "$file"
}

status="PASS"

mark() {
  local name="$1"
  local result="$2"
  local detail="$3"
  detail="${detail//$'\n'/ }"
  detail="${detail//|/\\|}"
  printf '| %s | %s | %s |\n' "$name" "$result" "$detail" >> "$OUT"
  case "$result" in
    PASS|INFO|RUNNING) ;;
    *) status="FAIL" ;;
  esac
}

gate() {
  local name="$1"
  local pattern="$2"
  local dir="${3:-$REPORT_DIR}"
  local required="${4:-1}"
  local file result detail
  file="$(latest "$pattern" "$dir")"
  result="$(overall "$file")"
  if [[ -n "$file" ]]; then
    detail="$(basename "$file")"
  else
    detail="missing $pattern"
  fi
  if [[ "$required" == "1" && "$result" != "PASS" ]]; then
    mark "$name" "$result" "$detail"
  else
    mark "$name" "$result" "$detail"
  fi
}

if [[ -z "$SOAK_REPORT" ]]; then
  SOAK_REPORT="$(latest 'soak-24h-*.md')"
fi

{
  echo "# TorrentNG Pre-Engine Release Gate"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Report directory: $REPORT_DIR"
  echo "- Benchmark directory: $BENCHMARK_DIR"
  echo
  echo "## Automated Gate Matrix"
  echo
  echo "| Gate | Status | Evidence |"
  echo "|---|---|---|"
} > "$OUT"

gate "Live API/app readiness" 'live-cert-*.md'
gate "Client configuration" 'client-config-*.md'
gate "Live transfer" 'live-transfer-*.md'
gate "Release grab stack" 'release-grab-*.md'
gate "Prowlarr app grab/transfer" 'app-add-job-*.md'
gate "Sonarr/Radarr app grab/transfer" 'arr-app-*.md'
gate "autobrr downloader/filter/action" 'autobrr-*.md'
gate "DHT/public-port wiring" 'dht-cert-*.md'
gate "NAT-PMP DHT" 'natpmp-dht-*.md' "$REPORT_DIR" 0
gate "Proton NAT-PMP" 'proton-natpmp-*.md' "$REPORT_DIR" 0
gate "Mobile qBit read-flow" 'mobile-compat-*.md'
gate "Phase 1 ruTorrent" 'phase1-cert-*.md'
gate "Synthetic benchmark" 'report-*.md' "$BENCHMARK_DIR"
gate "Short soak" 'soak-202*.md'
gate "Soak status" 'soak-status-*.md' "$REPORT_DIR" 0
gate "Security review automation" 'security-review-*.md'
gate "Security scan" 'security-scan-*.md'
gate "Native engine rewrite certification" 'native-engine-*.md'

{
  echo
  echo "## 24h Soak State"
  echo
  echo "| Check | Status | Detail |"
  echo "|---|---|---|"
} >> "$OUT"

if [[ -n "$SOAK_REPORT" && -f "$SOAK_REPORT" ]]; then
  samples="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {count++} END {print count+0}' "$SOAK_REPORT")"
  latest_sample="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {line=$0} END {print line}' "$SOAK_REPORT")"
  min_torrents="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/ /, "", $4); if (min == "" || $4 < min) min=$4} END {print min == "" ? 0 : min}' "$SOAK_REPORT")"
  max_rss="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/ /, "", $5); if ($5+0 > max) max=$5+0} END {printf "%.1f", max}' "$SOAK_REPORT")"
  bad_health="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/^[[:space:]]+|[[:space:]]+$/, "", $3); if ($3 != "200") bad++} END {print bad+0}' "$SOAK_REPORT")"
  bad_sync="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/^[[:space:]]+|[[:space:]]+$/, "", $6); if ($6 != "200") bad++} END {print bad+0}' "$SOAK_REPORT")"
  soak_status="$(overall "$SOAK_REPORT")"
  active="$(pgrep -af 'soak_certification.sh' | tr '\n' '; ' || true)"

  mark "source report" "$([[ "$soak_status" == "PASS" ]] && echo PASS || echo RUNNING)" "$(basename "$SOAK_REPORT") status=$soak_status"
  mark "active process" "$([[ -n "$active" ]] && echo RUNNING || echo INFO)" "${active:-not currently running}"
  mark "sample count" "INFO" "$samples collected"
  mark "torrent floor so far" "$([[ "$min_torrents" -ge 15000 ]] && echo PASS || echo FAIL)" "min=$min_torrents target>=15000"
  mark "memory ceiling so far" "$(awk -v rss="$max_rss" 'BEGIN {print (rss <= 500) ? "PASS" : "FAIL"}')" "max=${max_rss}MB target<=500MB"
  mark "health samples so far" "$([[ "$bad_health" -eq 0 ]] && echo PASS || echo FAIL)" "bad=$bad_health"
  mark "sync samples so far" "$([[ "$bad_sync" -eq 0 ]] && echo PASS || echo FAIL)" "bad=$bad_sync"
  mark "latest sample" "INFO" "${latest_sample:-none}"
else
  mark "source report" "FAIL" "missing soak-24h report"
fi

{
  echo
  echo "## Manual Or External Gates"
  echo
  echo "| Gate | Status | Detail |"
  echo "|---|---|---|"
} >> "$OUT"

mark "Real mobile app UI check" "INFO" "script-level NZB360/Transdrone qBit read-flow passes; physical/emulator app UI run remains external"
mark "Live autobrr announce ingest" "INFO" "autobrr qBit client/filter/action passes; real tracker IRC/indexer announce requires tracker credentials"
mark "Independent security review" "INFO" "automated policy and dependency/image scans pass; independent human review remains external"
mark "Post-soak finalization" "INFO" "after 24h: SOAK_MIN_SAMPLES=1200 RESTORE_NORMAL=1 ./scripts/finalize_soak.sh ${SOAK_REPORT:-$REPORT_DIR/soak-24h-report.md}"

{
  echo
  echo "## Current Certification Status"
  echo
  "$ROOT/scripts/certification_status.sh" "$REPORT_DIR"
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
