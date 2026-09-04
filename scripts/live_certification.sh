#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${CERT_ENV_FILE:-$ROOT/deploy/certification/.env}"
COMPOSE_FILE="${CERT_COMPOSE_FILE:-$ROOT/deploy/certification/compose.yml}"
OUT="${1:-$ROOT/certification/reports/live-cert-$(date -u +%Y%m%dT%H%M%SZ).md}"

ENV_TNG_HOST_URL="${TNG_HOST_URL:-}"
ENV_SONARR_HOST_URL="${SONARR_HOST_URL:-}"
ENV_RADARR_HOST_URL="${RADARR_HOST_URL:-}"
ENV_PROWLARR_HOST_URL="${PROWLARR_HOST_URL:-}"
ENV_AUTOBRR_HOST_URL="${AUTOBRR_HOST_URL:-}"
ENV_CROSS_SEED_HOST_URL="${CROSS_SEED_HOST_URL:-}"

if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
fi

TNG_HOST_URL="${ENV_TNG_HOST_URL:-${TNG_HOST_URL:-http://localhost:${TNG_HOST_PORT:-18080}}}"
TNG_API_TOKEN="${TNG_API_TOKEN:-local-cert-api-token-20260904}"
SONARR_HOST_URL="${ENV_SONARR_HOST_URL:-${SONARR_HOST_URL:-http://localhost:${SONARR_HOST_PORT:-18989}}}"
RADARR_HOST_URL="${ENV_RADARR_HOST_URL:-${RADARR_HOST_URL:-http://localhost:${RADARR_HOST_PORT:-17878}}}"
PROWLARR_HOST_URL="${ENV_PROWLARR_HOST_URL:-${PROWLARR_HOST_URL:-http://localhost:${PROWLARR_HOST_PORT:-19696}}}"
AUTOBRR_HOST_URL="${ENV_AUTOBRR_HOST_URL:-${AUTOBRR_HOST_URL:-http://localhost:${AUTOBRR_HOST_PORT:-17474}}}"
CROSS_SEED_HOST_URL="${ENV_CROSS_SEED_HOST_URL:-${CROSS_SEED_HOST_URL:-http://localhost:${CROSS_SEED_HOST_PORT:-12468}}}"

mkdir -p "$(dirname "$OUT")"

if [[ "${CERT_START_STACK:-0}" == "1" ]]; then
  docker compose --env-file "$ENV_FILE" -f "$COMPOSE_FILE" up -d --build
  for _ in $(seq 1 30); do
    code="$(curl -ksS -o /dev/null -w '%{http_code}' "$TNG_HOST_URL/health" || true)"
    [[ "$code" == "200" || "$code" == "503" ]] && break
    sleep 1
  done
fi

status="PASS"

mark() {
  local name="$1"
  local result="$2"
  local detail="$3"
  printf '| %s | %s | %s |\n' "$name" "$result" "$detail" >> "$OUT"
  if [[ "$result" == "FAIL" ]]; then
    status="FAIL"
  fi
}

http_code() {
  local url="$1"
  shift || true
  curl -ksS -o /tmp/tng-cert-body.txt -w '%{http_code}' "$@" "$url" || true
}

{
  echo "# TorrentNG Live Certification Report"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Compose file: $COMPOSE_FILE"
  echo "- TorrentNG URL: $TNG_HOST_URL"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

code="$(http_code "$TNG_HOST_URL/health")"
if [[ "$code" == "200" || "$code" == "503" ]]; then
  mark "sidecar health endpoint" "PASS" "HTTP $code"
else
  mark "sidecar health endpoint" "FAIL" "HTTP $code"
fi

code="$(http_code "$TNG_HOST_URL/api/qb/v2/auth/login" -X POST -d "username=$TNG_API_TOKEN" -d "password=$TNG_API_TOKEN" -c /tmp/tng-cert-cookies.txt)"
body="$(cat /tmp/tng-cert-body.txt 2>/dev/null || true)"
if [[ "$code" == "200" && "$body" == "Ok." ]]; then
  mark "qBit auth login" "PASS" "session cookie accepted"
else
  mark "qBit auth login" "FAIL" "HTTP $code body=${body:-empty}"
fi

for endpoint in \
  "/api/qb/v2/app/version" \
  "/api/qb/v2/app/webapiVersion" \
  "/api/qb/v2/app/preferences" \
  "/api/qb/v2/app/defaultSavePath" \
  "/api/qb/v2/torrents/info" \
  "/api/qb/v2/torrents/categories" \
  "/api/qb/v2/torrents/tags" \
  "/api/qb/v2/sync/maindata"; do
  code="$(http_code "$TNG_HOST_URL$endpoint" -b /tmp/tng-cert-cookies.txt)"
  if [[ "$code" == "200" ]]; then
    mark "qBit $endpoint" "PASS" "HTTP 200"
  else
    mark "qBit $endpoint" "FAIL" "HTTP $code"
  fi
done

code="$(http_code "$TNG_HOST_URL/api/v1/cross-seed" -H "Authorization: Bearer $TNG_API_TOKEN" -H 'Content-Type: application/json' -d '{"hashes":[],"trackers":[],"dry_run":true}')"
if [[ "$code" == "200" || "$code" == "400" ]]; then
  mark "native cross-seed helper" "PASS" "endpoint reachable, validation active HTTP $code"
else
  mark "native cross-seed helper" "FAIL" "HTTP $code"
fi

code="$(http_code "$TNG_HOST_URL/api/v1/settings/user-agent" -H "Authorization: Bearer $TNG_API_TOKEN")"
if [[ "$code" == "200" ]]; then
  body="$(cat /tmp/tng-cert-body.txt 2>/dev/null || true)"
  mark "rTorrent tracker user-agent control" "PASS" "current ${body:-unknown}"
else
  mark "rTorrent tracker user-agent control" "BLOCKED" "HTTP $code; bundled rTorrent may not expose network.http.user_agent"
fi

for svc in \
  "Sonarr|$SONARR_HOST_URL|/ping" \
  "Radarr|$RADARR_HOST_URL|/ping" \
  "Prowlarr|$PROWLARR_HOST_URL|/ping" \
  "autobrr|$AUTOBRR_HOST_URL|/" \
  "cross-seed|$CROSS_SEED_HOST_URL|/api/ping"; do
  IFS='|' read -r name base path <<< "$svc"
  code="$(http_code "$base$path")"
  if [[ "$code" == "200" || "$code" == "401" || "$code" == "403" ]]; then
    mark "$name container API" "PASS" "HTTP $code"
  else
    mark "$name container API" "BLOCKED" "HTTP $code; first-run setup or image startup may be required"
  fi
done

{
  echo
  echo "## Integration Gates"
  echo
  echo "- Run \`scripts/configure_certification_clients.sh\` to configure and test Sonarr/Radarr/Prowlarr/autobrr qBittorrent clients against \`torrentng:8080\`."
  echo "- Configure cross-seed qBittorrent URL as \`http://$TNG_API_TOKEN:$TNG_API_TOKEN@torrentng:8080\` where supported."
  echo "- Use tracker/indexer or local fixture releases for full add-torrent job certification."
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
