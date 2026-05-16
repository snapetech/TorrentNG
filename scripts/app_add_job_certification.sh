#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${CERT_ENV_FILE:-$ROOT/deploy/certification/.env}"
OUT="${1:-$ROOT/certification/reports/app-add-job-$(date -u +%Y%m%dT%H%M%SZ).md}"

if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
fi

RTNG_API_TOKEN="${RTNG_API_TOKEN:-cert-token}"
RTNG_HOST_URL="${RTNG_HOST_URL:-http://localhost:${RTNG_HOST_PORT:-18080}}"
PROWLARR_CONTAINER="${PROWLARR_CONTAINER:-certification-prowlarr-1}"
RTNG_CONTAINER="${RTNG_CONTAINER:-certification-rtorrentng-1}"
NETWORK="${CERT_DOCKER_NETWORK:-certification_default}"
DOWNLOADS_VOLUME="${CERT_DOWNLOADS_VOLUME:-certification_downloads}"
FIXTURE_BYTES="${FIXTURE_BYTES:-1048576}"
FIXTURE_ID="fixture-$(date -u +%Y%m%dT%H%M%SZ)"
TRACKER_NAME="rtng-app-tracker-$FIXTURE_ID"
INDEXER_NAME="rtng-fixture-indexer-$FIXTURE_ID"
SEEDER_NAME="rtng-app-seeder-$FIXTURE_ID"
COOKIE_JAR="$(mktemp)"
BODY="$(mktemp)"

mkdir -p "$(dirname "$OUT")"

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

RTNG_HOST_URL="$(mapped_host_url "$RTNG_HOST_URL" "$RTNG_CONTAINER" 8080)"

cleanup() {
  docker rm -f "$TRACKER_NAME" "$INDEXER_NAME" "$SEEDER_NAME" >/dev/null 2>&1 || true
  rm -f "$COOKIE_JAR" "$BODY"
}
trap cleanup EXIT

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
  docker exec "$1" sh -lc "sed -n 's:.*<ApiKey>\\(.*\\)</ApiKey>.*:\\1:p' /config/config.xml | head -1"
}

wait_for_torrent() {
  local name="$1"
  local deadline=$((SECONDS + 120))
  while (( SECONDS < deadline )); do
    row="$(curl -ksS -b "$COOKIE_JAR" "$RTNG_HOST_URL/api/qb/v2/torrents/info" \
      | jq -c --arg name "$name" '.[] | select(.name==$name) | select((.progress // 0) >= 1)' | head -1)"
    if [[ -n "$row" ]]; then
      printf '%s\n' "$row"
      return 0
    fi
    sleep 2
  done
  return 1
}

{
  echo "# rtorrentNG App-Driven Add Job Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- App path: Prowlarr Torznab search -> Prowlarr grab -> qBittorrent-compatible rtorrentNG client"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

code="$(curl -ksS -o "$BODY" -w '%{http_code}' "$RTNG_HOST_URL/api/qb/v2/auth/login" -X POST -d "username=$RTNG_API_TOKEN" -d "password=$RTNG_API_TOKEN" -c "$COOKIE_JAR")"
if [[ "$code" == "200" ]]; then
  mark "qBit auth" "PASS" "session cookie accepted"
else
  mark "qBit auth" "FAIL" "HTTP $code"
  echo >> "$OUT"; echo "Overall status: $status" >> "$OUT"; echo "$OUT"; exit 1
fi

docker run -d --rm --name "$TRACKER_NAME" --network "$NETWORK" lednerb/opentracker-docker >/dev/null
mark "local tracker" "PASS" "$TRACKER_NAME on $NETWORK"

docker run --rm --network "$NETWORK" -v "$DOWNLOADS_VOLUME:/downloads" alpine:3.20 sh -lc "
  apk add --no-cache mktorrent >/dev/null
  rm -rf /downloads/cert-fixture
  mkdir -p /downloads/cert-fixture/seed /downloads/cert-fixture/leech
  dd if=/dev/urandom of=/downloads/cert-fixture/seed/rtng-fixture.bin bs=$FIXTURE_BYTES count=1 status=none
  mktorrent -a http://$TRACKER_NAME:6969/announce -o /downloads/cert-fixture/rtng-fixture.torrent /downloads/cert-fixture/seed/rtng-fixture.bin >/dev/null
"
mark "fixture torrent" "PASS" "$FIXTURE_BYTES byte torrent generated"

docker run -d --rm --name "$INDEXER_NAME" --network "$NETWORK" \
  -v "$DOWNLOADS_VOLUME:/downloads" \
  -v "$ROOT/deploy/certification/fixture_indexer.py:/fixture_indexer.py:ro" \
  -e "FIXTURE_PUBLIC_BASE=http://$INDEXER_NAME:8082" \
  -e "FIXTURE_GUID=$FIXTURE_ID" \
  -e "FIXTURE_SIZE=$FIXTURE_BYTES" \
  python:3-alpine python /fixture_indexer.py >/dev/null

docker run -d --rm --name "$SEEDER_NAME" --network "$NETWORK" \
  -v "$DOWNLOADS_VOLUME:/downloads" \
  alpine:3.20 sh -lc 'apk add --no-cache transmission-cli >/dev/null && exec transmission-cli -w /downloads/cert-fixture/seed /downloads/cert-fixture/rtng-fixture.torrent' >/dev/null
mark "stock seeder" "PASS" "$SEEDER_NAME seeding fixture"

for _ in $(seq 1 30); do
  if docker run --rm --network "$NETWORK" alpine:3.20 wget -qO /dev/null "http://$INDEXER_NAME:8082/api?t=caps" >/dev/null 2>&1; then
    mark "fixture Torznab" "PASS" "caps reachable"
    break
  fi
  sleep 1
done

PROWLARR_API_KEY="${PROWLARR_API_KEY_OVERRIDE:-$(api_key_from_container "$PROWLARR_CONTAINER")}"
PROWLARR_BASE_URL="${PROWLARR_HOST_URL:-http://localhost:${PROWLARR_HOST_PORT:-19696}}"
PROWLARR_BASE_URL="$(mapped_host_url "$PROWLARR_BASE_URL" "$PROWLARR_CONTAINER" 9696)"

schema="$(curl -fsS -H "X-Api-Key: $PROWLARR_API_KEY" "$PROWLARR_BASE_URL/api/v1/indexer/schema" | jq 'map(select(.implementation=="Torznab"))[0]')"
payload="$(printf '%s' "$schema" | jq --arg base "http://$INDEXER_NAME:8082" --arg key "fixture" '
  .enable=true
  | .name="rtorrentNG Fixture Torznab"
  | .appProfileId=1
  | .fields |= map(
      if .name=="baseUrl" then .value=$base
      elif .name=="apiPath" then .value="/api"
      elif .name=="apiKey" then .value=$key
      elif .name=="torrentBaseSettings.appMinimumSeeders" then .value=1
      else . end
    )')"

existing_id="$(curl -fsS -H "X-Api-Key: $PROWLARR_API_KEY" "$PROWLARR_BASE_URL/api/v1/indexer" | jq -r '.[] | select(.name=="rtorrentNG Fixture Torznab") | .id' | head -1)"
if [[ -n "$existing_id" ]]; then
  payload="$(printf '%s' "$payload" | jq --argjson id "$existing_id" '.id=$id')"
  code="$(curl -ksS -o "$BODY" -w '%{http_code}' -H "X-Api-Key: $PROWLARR_API_KEY" -H 'Content-Type: application/json' -X PUT -d "$payload" "$PROWLARR_BASE_URL/api/v1/indexer/$existing_id")"
else
  code="$(curl -ksS -o "$BODY" -w '%{http_code}' -H "X-Api-Key: $PROWLARR_API_KEY" -H 'Content-Type: application/json' -X POST -d "$payload" "$PROWLARR_BASE_URL/api/v1/indexer")"
fi

if [[ "$code" == "200" || "$code" == "201" || "$code" == "202" ]]; then
  indexer_id="$(jq -r '.id' "$BODY")"
  mark "Prowlarr fixture indexer" "PASS" "indexer id $indexer_id"
else
  mark "Prowlarr fixture indexer" "FAIL" "HTTP $code $(tr '\n' ' ' <"$BODY")"
  echo >> "$OUT"; echo "Overall status: $status" >> "$OUT"; echo "$OUT"; exit 1
fi

search_body="$(curl -fsS -G -H "X-Api-Key: $PROWLARR_API_KEY" "$PROWLARR_BASE_URL/api/v1/search" --data-urlencode "query=rtng-fixture" --data-urlencode "indexerIds=$indexer_id")"
release="$(printf '%s' "$search_body" | jq -c '.[0] // empty')"
if [[ -n "$release" ]]; then
  mark "Prowlarr fixture search" "PASS" "$(printf '%s' "$release" | jq -r '.title')"
else
  mark "Prowlarr fixture search" "FAIL" "no releases returned"
  echo >> "$OUT"; echo "Overall status: $status" >> "$OUT"; echo "$OUT"; exit 1
fi

code="$(curl -ksS -o "$BODY" -w '%{http_code}' -H "X-Api-Key: $PROWLARR_API_KEY" -H 'Content-Type: application/json' -X POST -d "$release" "$PROWLARR_BASE_URL/api/v1/search")"
if [[ "$code" == "200" || "$code" == "201" || "$code" == "202" ]]; then
  mark "Prowlarr grab" "PASS" "release submitted to rtorrentNG qBit client"
else
  mark "Prowlarr grab" "FAIL" "HTTP $code $(tr '\n' ' ' <"$BODY")"
fi

if row="$(wait_for_torrent "rtng-fixture.bin")"; then
  mark "app-driven fixture transfer" "PASS" "$(jq -r '"progress=\(.progress) size=\(.size) downloaded=\(.downloaded)"' <<<"$row")"
else
  mark "app-driven fixture transfer" "FAIL" "fixture did not complete within timeout"
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
