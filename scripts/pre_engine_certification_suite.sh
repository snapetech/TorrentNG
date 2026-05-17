#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/pre-engine-suite-$(date -u +%Y%m%dT%H%M%SZ).md}"

mkdir -p "$REPORT_DIR"

status="PASS"

mark() {
  local name="$1"
  local result="$2"
  local report="$3"
  printf '| %s | %s | %s |\n' "$name" "$result" "$(basename "$report")" >> "$OUT"
  if [[ "$result" != "PASS" && "$result" != "INFO" && "$result" != "SKIP" ]]; then
    status="FAIL"
  fi
}

run_gate() {
  local name="$1"
  local report="$2"
  shift 2
  if "$@" "$report"; then
    mark "$name" "PASS" "$report"
  else
    mark "$name" "FAIL" "$report"
  fi
}

latest() {
  local pattern="$1"
  find "$REPORT_DIR" -maxdepth 1 -type f -name "$pattern" -printf '%T@ %p\n' 2>/dev/null \
    | sort -nr | awk 'NR==1 {print $2}'
}

active_long_soak() {
  pgrep -af 'soak_certification.sh' | grep -q 'soak-24h-' 2>/dev/null
}

mapped_port() {
  local container="$1"
  local port="$2"
  docker port "$container" "$port/tcp" 2>/dev/null | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -1 || true
}

derive_primary_stack_env() {
  local tng="${TNG_CONTAINER:-certification-torrentng-1}"
  local sonarr="${SONARR_CONTAINER:-certification-sonarr-1}"
  local radarr="${RADARR_CONTAINER:-certification-radarr-1}"
  local prowlarr="${PROWLARR_CONTAINER:-certification-prowlarr-1}"
  local autobrr="${AUTOBRR_CONTAINER:-certification-autobrr-1}"
  local cross_seed="${CROSS_SEED_CONTAINER:-certification-cross-seed-1}"
  local port

  port="$(mapped_port "$tng" 8080)"; [[ -n "$port" ]] && export TNG_HOST_URL="http://localhost:$port" TNG_HOST_PORT="$port"
  port="$(mapped_port "$tng" 50000)"; [[ -n "$port" ]] && export TNG_INCOMING_PORT="$port"
  port="$(mapped_port "$sonarr" 8989)"; [[ -n "$port" ]] && export SONARR_HOST_URL="http://localhost:$port" SONARR_HOST_PORT="$port"
  port="$(mapped_port "$radarr" 7878)"; [[ -n "$port" ]] && export RADARR_HOST_URL="http://localhost:$port" RADARR_HOST_PORT="$port"
  port="$(mapped_port "$prowlarr" 9696)"; [[ -n "$port" ]] && export PROWLARR_HOST_URL="http://localhost:$port" PROWLARR_HOST_PORT="$port"
  port="$(mapped_port "$autobrr" 7474)"; [[ -n "$port" ]] && export AUTOBRR_HOST_URL="http://localhost:$port" AUTOBRR_HOST_PORT="$port"
  port="$(mapped_port "$cross_seed" 2468)"; [[ -n "$port" ]] && export CROSS_SEED_HOST_URL="http://localhost:$port" CROSS_SEED_HOST_PORT="$port"
}

{
  echo "# TorrentNG Pre-Engine Certification Suite"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Scope: refresh all short/non-24h automated release evidence"
  echo "- Report directory: $REPORT_DIR"
  echo
  echo "## Gates"
  echo
  echo "| Gate | Result | Report |"
  echo "|---|---|---|"
} > "$OUT"

derive_primary_stack_env

run_gate "live API/app readiness" "$REPORT_DIR/live-cert-suite-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/live_certification.sh"
run_gate "universal compatibility local gates" "$REPORT_DIR/universal-compat-suite-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/universal_compatibility_certification.sh"
run_gate "client configuration" "$REPORT_DIR/client-config-suite-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/configure_certification_clients.sh"
if active_long_soak && [[ "${SUITE_ALLOW_PRIMARY_MUTATION:-0}" != "1" ]]; then
  mark "live transfer" "SKIP" "$(latest 'live-transfer-*.md')"
else
  run_gate "live transfer" "$REPORT_DIR/live-transfer-suite-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/live_transfer_certification.sh"
fi
run_gate "release grab stack" "$REPORT_DIR/release-grab-suite-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/release_grab_certification.sh"
run_gate "Docker interop local matrix" "$REPORT_DIR/interop-local-suite-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/interop_matrix.sh" --local
run_gate "DHT/public-port wiring" "$REPORT_DIR/dht-cert-suite-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/dht_certification.sh"
run_gate "mobile qBit read-flow" "$REPORT_DIR/mobile-compat-suite-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/mobile_compat_certification.sh"
run_gate "Phase 1 ruTorrent" "$REPORT_DIR/phase1-cert-suite-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/phase1_certification.sh"
security_review_report="$REPORT_DIR/security-review-suite-$(date -u +%Y%m%dT%H%M%SZ).md"
if TNG_API_TOKENS="${TNG_API_TOKENS:-suite-token}" TNG_SECRET_KEY="${TNG_SECRET_KEY:-suite-secret-00000000000000000000000000000000}" "$ROOT/scripts/security_review.sh" "$ROOT/deploy/docker/sidecar.config.toml" "$security_review_report"; then
  mark "security review automation" "PASS" "$security_review_report"
else
  mark "security review automation" "FAIL" "$security_review_report"
fi
run_gate "security scan" "$REPORT_DIR/security-scan-suite-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/security_scan.sh"
run_gate "native engine rewrite certification" "$REPORT_DIR/native-engine-suite-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/native_engine_certification_report.sh"
run_gate "pre-engine release report" "$REPORT_DIR/pre-engine-release-suite-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/pre_engine_release_report.sh"

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
