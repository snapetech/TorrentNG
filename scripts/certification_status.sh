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
row "Synthetic benchmark" 'report-*.md' "$BENCHMARK_DIR"
row "Short soak" 'soak-202*.md'
row "Transfer churn soak" 'transfer-churn-*.md'
row "24h soak" 'soak-24h-*.md'
row "Soak status" 'soak-status-*.md'
row "Soak finalization" 'soak-final-*.md'
row "Security review" 'security-review-*.md'
row "Security scan" 'security-scan-*.md'
row "Native engine rewrite" 'native-engine-*.md'
row "Pre-engine release report" 'pre-engine-release-*.md'
row "Pre-engine suite" 'pre-engine-suite-*.md'
row "Post-soak release gate" 'post-soak-release-*.md'

echo
echo "## Active Long-Running Jobs"
echo
pgrep -af 'soak_certification.sh' || true
