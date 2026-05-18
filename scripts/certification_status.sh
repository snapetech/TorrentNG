#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${1:-$ROOT/certification/reports}"
BENCHMARK_DIR="${BENCHMARK_DIR:-$ROOT/benchmarks}"

latest() {
  local pattern="$1"
  local dir="${2:-$REPORT_DIR}"
  find "$dir" -maxdepth 1 -type f -name "$pattern" -printf '%T@ %p\n' 2>/dev/null \
    | sort -nr | awk 'NR==1 {print $2}'
}

latest_excluding() {
  local pattern="$1"
  local dir="$2"
  shift 2
  local excludes=("$@")
  find "$dir" -maxdepth 1 -type f -name "$pattern" -printf '%T@ %p\n' 2>/dev/null \
    | while read -r ts path; do
        local base skip exclude
        base="$(basename "$path")"
        skip=0
        for exclude in "${excludes[@]}"; do
          if [[ "$base" == $exclude ]]; then
            skip=1
            break
          fi
        done
        [[ "$skip" == "1" ]] || printf '%s %s\n' "$ts" "$path"
      done \
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

row() {
  local name="$1"
  local pattern="$2"
  local dir="${3:-$REPORT_DIR}"
  local file status sample
  file="$(latest "$pattern" "$dir")"
  status="$(overall "$file")"
  sample="-"
  if [[ -n "$file" && -f "$file" ]]; then
    sample="$(basename "$file")"
  fi
  printf '| %s | %s | %s |\n' "$name" "$status" "$sample"
}

row_excluding() {
  local name="$1"
  local pattern="$2"
  local dir="$3"
  shift 3
  local file status sample
  file="$(latest_excluding "$pattern" "$dir" "$@")"
  status="$(overall "$file")"
  sample="-"
  if [[ -n "$file" && -f "$file" ]]; then
    sample="$(basename "$file")"
  fi
  printf '| %s | %s | %s |\n' "$name" "$status" "$sample"
}

row_24h_soak() {
  local file status sample active
  file="$(latest 'soak-24h-*.md')"
  status="$(overall "$file")"
  sample="-"
  if [[ -n "$file" && -f "$file" ]]; then
    sample="$(basename "$file")"
    if [[ "$status" == "RUNNING/UNKNOWN" ]]; then
      active="$(pgrep -af '[s]oak_certification.sh' | grep -F "$sample" || true)"
      if [[ -n "$active" ]]; then
        status="RUNNING"
      else
        status="STALE/INCOMPLETE"
      fi
    fi
  fi
  printf '| %s | %s | %s |\n' "24h soak" "$status" "$sample"
}

storage_index_has() {
  local file="$1"
  local kind="$2"
  local result="$3"
  awk -F'|' -v kind="$kind" -v result="$result" '
    NR <= 2 { next }
    {
      for (i = 1; i <= NF; i++) {
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", $i)
      }
      if ($3 == kind && $NF == "" && $(NF - 1) == result) found = 1
      else if ($3 == kind && $NF == result) found = 1
    }
    END { exit found ? 0 : 1 }
  ' "$file"
}

storage_index_row() {
  local name="$1"
  local kind="$2"
  local index sample status
  index="$(latest 'storage-certification-index.md')"
  sample="-"
  if [[ -z "$index" || ! -f "$index" ]]; then
    status="MISSING"
  else
    sample="$(basename "$index")"
    if storage_index_has "$index" "$kind" PASS; then
      status="PASS"
    elif storage_index_has "$index" "$kind" FAIL; then
      status="FAIL"
    elif storage_index_has "$index" "$kind" SKIP; then
      status="SKIP"
    else
      status="MISSING"
    fi
  fi
  printf '| %s | %s | %s |\n' "$name" "$status" "$sample"
}

echo "# TorrentNG Certification Status"
echo
echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "- Report directory: $REPORT_DIR"
echo "- Benchmark directory: $BENCHMARK_DIR"
echo
echo "| Gate | Status | Latest report |"
echo "|---|---|---|"
row "Live API/app readiness" 'live-cert-*.md'
row "Client configuration" 'client-config-*.md'
row "Live transfer" 'live-transfer-*.md'
row "Release grab stack" 'release-grab-*.md'
row "App-driven Prowlarr add" 'app-add-job-*.md'
row "Sonarr/Radarr app grab" 'arr-app-*.md'
row "autobrr filter/action" 'autobrr-*.md'
row "DHT" 'dht-cert-*.md'
row "NAT-PMP DHT" 'natpmp-dht-*.md'
row "Proton NAT-PMP" 'proton-natpmp-*.md'
row "Proton-routed TorrentNG DHT" 'proton-tng-dht-*.md'
row "Mobile read-flow" 'mobile-compat-*.md'
row "Phase 1 ruTorrent" 'phase1-cert-*.md'
row "Universal compatibility" 'universal-compat-*.md'
row "Universal live compatibility" 'universal-live-*.md'
row_excluding "Migration corpus" 'migration-corpus-*.md' "$REPORT_DIR" 'migration-corpus-local-release-*' 'migration-corpus-universal-*'
row "External evidence preflight" 'external-evidence-preflight-*.md'
row "Synthetic benchmark" 'report-*.md' "$BENCHMARK_DIR"
row "Short soak" 'soak-202*.md'
row "Transfer churn soak" 'transfer-churn-*.md'
row_24h_soak
row "Soak status" 'soak-status-*.md'
row "Soak finalization" 'soak-final-*.md'
row "Security review" 'security-review-*.md'
row "Security scan" 'security-scan-*.md'
row "Native engine rewrite" 'native-engine-*.md'
row "WebUI certification" 'webui-certification-*.md'
row "Local release gate" 'local-release-*.md'
row "Storage hardware matrix" 'storage-hardware-*.md'
row "Storage io_uring capability/graduation" 'storage-uring-graduation-*.md'
row "Storage move/import" 'storage-move-import-*.md'
row "Storage release certification" 'storage-release-certification-*.md'
storage_index_row "Storage indexed hardware evidence" 'hardware matrix'
storage_index_row "Storage indexed io_uring evidence" 'io_uring capability/graduation'
storage_index_row "Storage indexed move/import evidence" 'move/import'
row "Pre-engine release report" 'pre-engine-release-*.md'
row "Pre-engine suite" 'pre-engine-suite-*.md'
row "Post-soak release gate" 'post-soak-release-*.md'
row "Certification burndown" 'certification-burndown-*.md'
row "Release readiness" 'release-readiness-*.md'
row "Certification bundle" 'certification-bundle-*.md'
row "Release evidence suite" 'release-evidence-suite-*.md'
row "Certification JSON status" 'certification-status-*.md'

echo
echo "## Active Long-Running Jobs"
echo
pgrep -af '[s]oak_certification.sh' || true
