#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${CERT_ENV_FILE:-$ROOT/deploy/certification/.env}"
COMPOSE_FILE="${CERT_COMPOSE_FILE:-$ROOT/deploy/certification/compose.yml}"
PROJECT="${CERT_GRAB_PROJECT:-certgrab}"
OUT="${1:-$ROOT/certification/reports/release-grab-$(date -u +%Y%m%dT%H%M%SZ).md}"
WORK_DIR="$(mktemp -d)"

mkdir -p "$(dirname "$OUT")"

cleanup() {
  if [[ "${CERT_GRAB_KEEP_STACK:-0}" != "1" ]]; then
    docker compose --env-file "$WORK_DIR/env" -p "$PROJECT" -f "$COMPOSE_FILE" down -v --remove-orphans >/dev/null 2>&1 || true
  fi
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

if [[ -f "$ENV_FILE" ]]; then
  cp "$ENV_FILE" "$WORK_DIR/env"
else
  cp "$ROOT/deploy/certification/.env.example" "$WORK_DIR/env"
fi

cat >> "$WORK_DIR/env" <<EOF

TNG_HOST_PORT=${CERT_GRAB_TNG_HOST_PORT:-38080}
TNG_INCOMING_PORT=${CERT_GRAB_TNG_INCOMING_PORT:-52000}
SONARR_HOST_PORT=${CERT_GRAB_SONARR_HOST_PORT:-38989}
RADARR_HOST_PORT=${CERT_GRAB_RADARR_HOST_PORT:-37878}
PROWLARR_HOST_PORT=${CERT_GRAB_PROWLARR_HOST_PORT:-39696}
AUTOBRR_HOST_PORT=${CERT_GRAB_AUTOBRR_HOST_PORT:-37474}
CROSS_SEED_HOST_PORT=${CERT_GRAB_CROSS_SEED_HOST_PORT:-32468}
TNG_SYNC_INTERVAL_SECS=2
EOF

set -a
# shellcheck disable=SC1090
source "$WORK_DIR/env"
set +a

export CERT_ENV_FILE="$WORK_DIR/env"
export CERT_COMPOSE_FILE="$COMPOSE_FILE"
export CERT_DOCKER_NETWORK="${PROJECT}_default"
export CERT_DOWNLOADS_VOLUME="${PROJECT}_downloads"
export TNG_CONTAINER="${PROJECT}-torrentng-1"
export SONARR_CONTAINER="${PROJECT}-sonarr-1"
export RADARR_CONTAINER="${PROJECT}-radarr-1"
export PROWLARR_CONTAINER="${PROJECT}-prowlarr-1"
export AUTOBRR_CONTAINER="${PROJECT}-autobrr-1"
export TNG_HOST_URL="http://localhost:${TNG_HOST_PORT:-38080}"
export SONARR_HOST_URL="http://localhost:${SONARR_HOST_PORT:-38989}"
export RADARR_HOST_URL="http://localhost:${RADARR_HOST_PORT:-37878}"
export PROWLARR_HOST_URL="http://localhost:${PROWLARR_HOST_PORT:-39696}"
export AUTOBRR_HOST_URL="http://localhost:${AUTOBRR_HOST_PORT:-37474}"
export CROSS_SEED_HOST_URL="http://localhost:${CROSS_SEED_HOST_PORT:-32468}"

status="PASS"

mark() {
  local gate="$1"
  local result="$2"
  local report="$3"
  printf '| %s | %s | %s |\n' "$gate" "$result" "$(basename "$report")" >> "$OUT"
  if [[ "$result" != "PASS" ]]; then
    status="FAIL"
  fi
}

run_gate() {
  local gate="$1"
  local report="$2"
  shift 2
  if "$@" "$report"; then
    mark "$gate" "PASS" "$report"
  else
    mark "$gate" "FAIL" "$report"
  fi
}

wait_for_stack() {
  local deadline=$((SECONDS + 240))
  while (( SECONDS < deadline )); do
    code="$(curl -ksS -o /dev/null -w '%{http_code}' "$TNG_HOST_URL/health" || true)"
    sonarr="$(curl -ksS -o /dev/null -w '%{http_code}' "$SONARR_HOST_URL/ping" || true)"
    radarr="$(curl -ksS -o /dev/null -w '%{http_code}' "$RADARR_HOST_URL/ping" || true)"
    prowlarr="$(curl -ksS -o /dev/null -w '%{http_code}' "$PROWLARR_HOST_URL/ping" || true)"
    autobrr="$(curl -ksS -o /dev/null -w '%{http_code}' "$AUTOBRR_HOST_URL/" || true)"
    if [[ "$code" =~ ^(200|503)$ && "$sonarr" == "200" && "$radarr" == "200" && "$prowlarr" == "200" && "$autobrr" =~ ^(200|401|403)$ ]]; then
      return 0
    fi
    sleep 4
  done
  return 1
}

{
  echo "# TorrentNG Release Grab Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Compose project: $PROJECT"
  echo "- Scope: isolated normal-sync stack for app-driven release grabs while the primary soak stack can continue running"
  echo "- TorrentNG URL: $TNG_HOST_URL"
  echo
  echo "## Gates"
  echo
  echo "| Gate | Result | Report |"
  echo "|---|---|---|"
} > "$OUT"

docker compose --env-file "$WORK_DIR/env" -p "$PROJECT" -f "$COMPOSE_FILE" up -d --build

if wait_for_stack; then
  mark "isolated stack readiness" "PASS" "$OUT"
else
  mark "isolated stack readiness" "FAIL" "$OUT"
  echo >> "$OUT"
  echo "Overall status: $status" >> "$OUT"
  echo "$OUT"
  exit 1
fi

run_gate "live API/app readiness" "$ROOT/certification/reports/live-cert-${PROJECT}-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/live_certification.sh"
run_gate "client configuration" "$ROOT/certification/reports/client-config-${PROJECT}-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/configure_certification_clients.sh"
run_gate "Prowlarr release grab/transfer" "$ROOT/certification/reports/app-add-job-${PROJECT}-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/app_add_job_certification.sh"
ARR_GRAB=1 run_gate "Sonarr/Radarr release grab/transfer" "$ROOT/certification/reports/arr-app-${PROJECT}-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/arr_app_certification.sh"
run_gate "autobrr downloader/filter/action" "$ROOT/certification/reports/autobrr-${PROJECT}-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/autobrr_certification.sh"

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
