#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${CERT_ENV_FILE:-$ROOT/deploy/certification/.env}"
OUT="${1:-$ROOT/certification/reports/client-config-$(date -u +%Y%m%dT%H%M%SZ).md}"

if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
fi

TNG_API_TOKEN="${TNG_API_TOKEN:-local-cert-api-token-20260904}"
SONARR_HOST_URL="${SONARR_HOST_URL:-http://localhost:${SONARR_HOST_PORT:-18989}}"
RADARR_HOST_URL="${RADARR_HOST_URL:-http://localhost:${RADARR_HOST_PORT:-17878}}"
PROWLARR_HOST_URL="${PROWLARR_HOST_URL:-http://localhost:${PROWLARR_HOST_PORT:-19696}}"
AUTOBRR_HOST_URL="${AUTOBRR_HOST_URL:-http://localhost:${AUTOBRR_HOST_PORT:-17474}}"

SONARR_CONTAINER="${SONARR_CONTAINER:-certification-sonarr-1}"
RADARR_CONTAINER="${RADARR_CONTAINER:-certification-radarr-1}"
PROWLARR_CONTAINER="${PROWLARR_CONTAINER:-certification-prowlarr-1}"
AUTOBRR_CONTAINER="${AUTOBRR_CONTAINER:-certification-autobrr-1}"

mapped_host_url() {
  local current="$1"
  local container="$2"
  local container_port="$3"
  local mapped

  mapped="$(docker port "$container" "$container_port/tcp" 2>/dev/null | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -1 || true)"
  if [[ -n "$mapped" && "$current" == http://localhost:* ]]; then
    printf 'http://localhost:%s\n' "$mapped"
  else
    printf '%s\n' "$current"
  fi
}

SONARR_HOST_URL="$(mapped_host_url "$SONARR_HOST_URL" "$SONARR_CONTAINER" 8989)"
RADARR_HOST_URL="$(mapped_host_url "$RADARR_HOST_URL" "$RADARR_CONTAINER" 7878)"
PROWLARR_HOST_URL="$(mapped_host_url "$PROWLARR_HOST_URL" "$PROWLARR_CONTAINER" 9696)"
AUTOBRR_HOST_URL="$(mapped_host_url "$AUTOBRR_HOST_URL" "$AUTOBRR_CONTAINER" 7474)"

mkdir -p "$(dirname "$OUT")"

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

api_key_from_container() {
  local container="$1"
  docker exec "$container" sh -lc "sed -n 's:.*<ApiKey>\\(.*\\)</ApiKey>.*:\\1:p' /config/config.xml | head -1"
}

arr_payload() {
  local base_url="$1"
  local api_key="$2"
  local api_path="$3"
  local name="$4"
  local category_field="$5"
  local category_value="$6"

  curl -fsS -H "X-Api-Key: $api_key" "$base_url$api_path/downloadclient/schema" \
    | jq --arg name "$name" --arg category_field "$category_field" --arg category_value "$category_value" --arg token "$TNG_API_TOKEN" '
      map(select(.implementation=="QBittorrent"))[0]
      | .enable=true
      | .name=$name
      | .priority=1
      | .fields |= map(
          if .name=="host" then .value="torrentng"
          elif .name=="port" then .value=8080
          elif .name=="useSsl" then .value=false
          elif .name=="urlBase" then .value=""
          elif .name=="username" then .value=$token
          elif .name=="password" then .value=$token
          elif .name==$category_field then .value=$category_value
          elif .name=="initialState" then .value=0
          elif .name=="sequentialOrder" then .value=false
          elif .name=="firstAndLast" then .value=false
          elif .name=="contentLayout" then .value=0
          else . end
        )'
}

configure_arr_client() {
  local label="$1"
  local base_url="$2"
  local api_key="$3"
  local api_path="$4"
  local category_field="$5"
  local category_value="$6"
  local name="TorrentNG-qBit"
  local payload code existing_id

  payload="$(arr_payload "$base_url" "$api_key" "$api_path" "$name" "$category_field" "$category_value")"
  existing_id="$(curl -fsS -H "X-Api-Key: $api_key" "$base_url$api_path/downloadclient" | jq -r --arg name "$name" '.[] | select(.name==$name) | .id' | head -1)"
  if [[ -n "$existing_id" ]]; then
    payload="$(printf '%s' "$payload" | jq --argjson id "$existing_id" '.id=$id')"
  fi

  code="$(curl -ksS -o /tmp/tng-arr-test-body.txt -w '%{http_code}' -H "X-Api-Key: $api_key" -H 'Content-Type: application/json' -X POST -d "$payload" "$base_url$api_path/downloadclient/test")"
  if [[ "$code" != "200" ]]; then
    mark "$label qBit test" "FAIL" "HTTP $code $(tr '\n' ' ' </tmp/tng-arr-test-body.txt)"
    return
  fi

  if [[ -n "$existing_id" ]]; then
    code="$(curl -ksS -o /tmp/tng-arr-save-body.txt -w '%{http_code}' -H "X-Api-Key: $api_key" -H 'Content-Type: application/json' -X PUT -d "$payload" "$base_url$api_path/downloadclient/$existing_id")"
  else
    code="$(curl -ksS -o /tmp/tng-arr-save-body.txt -w '%{http_code}' -H "X-Api-Key: $api_key" -H 'Content-Type: application/json' -X POST -d "$payload" "$base_url$api_path/downloadclient")"
  fi

  if [[ "$code" == "200" || "$code" == "201" || "$code" == "202" ]]; then
    mark "$label qBit client" "PASS" "tested and saved as $name"
  else
    mark "$label qBit client" "FAIL" "HTTP $code $(tr '\n' ' ' </tmp/tng-arr-save-body.txt)"
  fi
}

configure_autobrr_client() {
  local cookies="/tmp/tng-autobrr-cookies.txt"
  local user="${AUTOBRR_CERT_USER:-cert}"
  local pass="${AUTOBRR_CERT_PASSWORD:-cert}"
  local payload code existing_id

  code="$(curl -ksS -o /tmp/tng-autobrr-onboard.txt -w '%{http_code}' "$AUTOBRR_HOST_URL/api/auth/onboard")"
  if [[ "$code" == "204" ]]; then
    curl -ksS -o /tmp/tng-autobrr-onboard.txt -H 'Content-Type: application/json' -X POST -d "{\"username\":\"$user\",\"password\":\"$pass\"}" "$AUTOBRR_HOST_URL/api/auth/onboard" >/dev/null
  fi

  code="$(curl -ksS -c "$cookies" -o /tmp/tng-autobrr-login.txt -w '%{http_code}' -H 'Content-Type: application/json' -X POST -d "{\"username\":\"$user\",\"password\":\"$pass\",\"remember_me\":true}" "$AUTOBRR_HOST_URL/api/auth/login")"
  if [[ "$code" != "204" ]]; then
    mark "autobrr login" "FAIL" "HTTP $code $(tr '\n' ' ' </tmp/tng-autobrr-login.txt)"
    return
  fi

  payload="$(jq -nc --arg token "$TNG_API_TOKEN" '{
    name: "TorrentNG-qBit",
    type: "QBITTORRENT",
    enabled: true,
    host: "torrentng",
    port: 8080,
    tls: false,
    tls_skip_verify: false,
    username: $token,
    password: $token,
    settings: {
      basic: {auth: false},
      rules: {enabled: false}
    }
  }')"

  code="$(curl -ksS -b "$cookies" -o /tmp/tng-autobrr-test.txt -w '%{http_code}' -H 'Content-Type: application/json' -X POST -d "$payload" "$AUTOBRR_HOST_URL/api/download_clients/test")"
  if [[ "$code" != "204" ]]; then
    mark "autobrr qBit test" "FAIL" "HTTP $code $(tr '\n' ' ' </tmp/tng-autobrr-test.txt)"
    return
  fi

  existing_id="$(curl -fsS -b "$cookies" "$AUTOBRR_HOST_URL/api/download_clients" | jq -r '.[] | select(.name=="TorrentNG-qBit") | .id' | head -1)"
  if [[ -n "$existing_id" ]]; then
    payload="$(printf '%s' "$payload" | jq --argjson id "$existing_id" '.id=$id')"
    code="$(curl -ksS -b "$cookies" -o /tmp/tng-autobrr-save.txt -w '%{http_code}' -H 'Content-Type: application/json' -X PUT -d "$payload" "$AUTOBRR_HOST_URL/api/download_clients")"
  else
    code="$(curl -ksS -b "$cookies" -o /tmp/tng-autobrr-save.txt -w '%{http_code}' -H 'Content-Type: application/json' -X POST -d "$payload" "$AUTOBRR_HOST_URL/api/download_clients")"
  fi

  if [[ "$code" == "200" || "$code" == "201" ]]; then
    mark "autobrr qBit client" "PASS" "tested and saved as TorrentNG-qBit"
  else
    mark "autobrr qBit client" "FAIL" "HTTP $code $(tr '\n' ' ' </tmp/tng-autobrr-save.txt)"
  fi
}

{
  echo "# TorrentNG Client Configuration Report"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- TorrentNG Docker host: torrentng:8080"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

SONARR_API_KEY="${SONARR_API_KEY_OVERRIDE:-$(api_key_from_container "$SONARR_CONTAINER")}"
RADARR_API_KEY="${RADARR_API_KEY_OVERRIDE:-$(api_key_from_container "$RADARR_CONTAINER")}"
PROWLARR_API_KEY="${PROWLARR_API_KEY_OVERRIDE:-$(api_key_from_container "$PROWLARR_CONTAINER")}"

configure_arr_client "Sonarr" "$SONARR_HOST_URL" "$SONARR_API_KEY" "/api/v3" "tvCategory" "tv-sonarr"
configure_arr_client "Radarr" "$RADARR_HOST_URL" "$RADARR_API_KEY" "/api/v3" "movieCategory" "radarr"
configure_arr_client "Prowlarr" "$PROWLARR_HOST_URL" "$PROWLARR_API_KEY" "/api/v1" "category" "prowlarr"
configure_autobrr_client

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
