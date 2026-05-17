#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${CERT_ENV_FILE:-$ROOT/deploy/certification/.env}"
OUT="${1:-$ROOT/certification/reports/arr-app-$(date -u +%Y%m%dT%H%M%SZ).md}"

if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
fi

TNG_CONTAINER="${TNG_CONTAINER:-certification-torrentng-1}"
SONARR_CONTAINER="${SONARR_CONTAINER:-certification-sonarr-1}"
RADARR_CONTAINER="${RADARR_CONTAINER:-certification-radarr-1}"
NETWORK="${CERT_DOCKER_NETWORK:-certification_default}"
DOWNLOADS_VOLUME="${CERT_DOWNLOADS_VOLUME:-certification_downloads}"
FIXTURE_BYTES="${FIXTURE_BYTES:-1048576}"
ARR_GRAB="${ARR_GRAB:-0}"
FIXTURE_ID="arr-$(date -u +%Y%m%dT%H%M%SZ)"
TRACKER_NAME="tng-arr-tracker-$FIXTURE_ID"
SONARR_SEEDER_NAME="tng-arr-sonarr-seeder-$FIXTURE_ID"
RADARR_SEEDER_NAME="tng-arr-radarr-seeder-$FIXTURE_ID"
SONARR_INDEXER_NAME="TorrentNG Sonarr Fixture"
RADARR_INDEXER_NAME="TorrentNG Radarr Fixture"
SONARR_INDEXER_CONTAINER="tng-sonarr-indexer-$FIXTURE_ID"
RADARR_INDEXER_CONTAINER="tng-radarr-indexer-$FIXTURE_ID"
COOKIE_JAR="$(mktemp)"
BODY="$(mktemp)"

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

cleanup() {
  docker rm -f "$TRACKER_NAME" "$SONARR_SEEDER_NAME" "$RADARR_SEEDER_NAME" "$SONARR_INDEXER_CONTAINER" "$RADARR_INDEXER_CONTAINER" >/dev/null 2>&1 || true
  rm -f "$COOKIE_JAR" "$BODY"
}
trap cleanup EXIT

api_key_from_container() {
  docker exec "$1" sh -lc "sed -n 's:.*<ApiKey>\\(.*\\)</ApiKey>.*:\\1:p' /config/config.xml | head -1"
}

mapped_host_url() {
  local container="$1"
  local port="$2"
  local mapped
  mapped="$(docker port "$container" "$port/tcp" 2>/dev/null | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -1 || true)"
  printf 'http://localhost:%s\n' "${mapped:-$port}"
}

delete_named_indexer() {
  local base="$1"
  local key="$2"
  local name="$3"
  curl -fsS -H "X-Api-Key: $key" "$base/api/v3/indexer" \
    | jq -r --arg name "$name" '.[] | select(.name==$name) | .id' \
    | while read -r id; do
        [[ -n "$id" ]] && curl -ksS -o /dev/null -H "X-Api-Key: $key" -X DELETE "$base/api/v3/indexer/$id"
      done
}

ensure_root_folder() {
  local label="$1"
  local base="$2"
  local key="$3"
  local path="$4"
  local container="$5"
  docker exec "$container" sh -lc "chown -R abc:abc '$path' /downloads || true"
  if curl -fsS -H "X-Api-Key: $key" "$base/api/v3/rootfolder" | jq -e --arg path "$path" '.[] | select(.path==$path)' >/dev/null; then
    mark "$label root folder" "PASS" "$path already configured"
    return
  fi
  code="$(curl -ksS -o "$BODY" -w '%{http_code}' -H "X-Api-Key: $key" -H 'Content-Type: application/json' -X POST -d "{\"path\":\"$path\"}" "$base/api/v3/rootfolder")"
  if [[ "$code" == "200" || "$code" == "201" ]]; then
    mark "$label root folder" "PASS" "$path"
  else
    mark "$label root folder" "FAIL" "HTTP $code $(tr '\n' ' ' <"$BODY")"
  fi
}

ensure_sonarr_series() {
  local base="$1"
  local key="$2"
  local series code
  if curl -fsS -H "X-Api-Key: $key" "$base/api/v3/series" | jq -e '.[] | select(.title=="Breaking Bad")' >/dev/null; then
    mark "Sonarr series" "PASS" "Breaking Bad already configured"
    return
  fi
  for _ in $(seq 1 5); do
    series="$(curl -fsS -H "X-Api-Key: $key" "$base/api/v3/series/lookup?term=Breaking%20Bad" 2>/dev/null \
      | jq '.[0] | .qualityProfileId=1 | .languageProfileId=1 | .rootFolderPath="/tv" | .monitored=false | .seasonFolder=true | .addOptions={monitor:"none", searchForMissingEpisodes:false}' 2>/dev/null || true)"
    [[ -n "$series" && "$series" != "null" ]] && break
    sleep 3
  done
  if [[ -z "${series:-}" || "$series" == "null" ]]; then
    mark "Sonarr series" "FAIL" "Breaking Bad lookup unavailable"
    return
  fi
  code="$(curl -ksS -o "$BODY" -w '%{http_code}' -H "X-Api-Key: $key" -H 'Content-Type: application/json' -X POST -d "$series" "$base/api/v3/series")"
  if [[ "$code" == "200" || "$code" == "201" ]]; then
    mark "Sonarr series" "PASS" "Breaking Bad"
  else
    mark "Sonarr series" "FAIL" "HTTP $code $(tr '\n' ' ' <"$BODY")"
  fi
}

ensure_radarr_movie() {
  local base="$1"
  local key="$2"
  local movie code
  if curl -fsS -H "X-Api-Key: $key" "$base/api/v3/movie" | jq -e '.[] | select(.title=="The Matrix")' >/dev/null; then
    mark "Radarr movie" "PASS" "The Matrix already configured"
    return
  fi
  for _ in $(seq 1 5); do
    movie="$(curl -fsS -H "X-Api-Key: $key" "$base/api/v3/movie/lookup?term=The%20Matrix" 2>/dev/null \
      | jq '.[0] | .qualityProfileId=1 | .rootFolderPath="/movies" | .monitored=false | .addOptions={searchForMovie:false}' 2>/dev/null || true)"
    [[ -n "$movie" && "$movie" != "null" ]] && break
    sleep 3
  done
  if [[ -z "${movie:-}" || "$movie" == "null" ]]; then
    mark "Radarr movie" "FAIL" "The Matrix lookup unavailable"
    return
  fi
  code="$(curl -ksS -o "$BODY" -w '%{http_code}' -H "X-Api-Key: $key" -H 'Content-Type: application/json' -X POST -d "$movie" "$base/api/v3/movie")"
  if [[ "$code" == "200" || "$code" == "201" ]]; then
    mark "Radarr movie" "PASS" "The Matrix"
  else
    mark "Radarr movie" "FAIL" "HTTP $code $(tr '\n' ' ' <"$BODY")"
  fi
}

create_indexer() {
  local label="$1"
  local base="$2"
  local key="$3"
  local name="$4"
  local indexer_url="$5"
  local categories="$6"

  delete_named_indexer "$base" "$key" "$name"
  schema="$(curl -fsS -H "X-Api-Key: $key" "$base/api/v3/indexer/schema" | jq 'map(select(.implementation=="Torznab"))[0]')"
  payload="$(printf '%s' "$schema" | jq --arg name "$name" --arg base "$indexer_url" --argjson cats "$categories" '
    .enable=true
    | .enableRss=true
    | .enableAutomaticSearch=true
    | .enableInteractiveSearch=true
    | .name=$name
    | .fields |= map(
        if .name=="baseUrl" then .value=$base
        elif .name=="apiPath" then .value="/api"
        elif .name=="apiKey" then .value="fixture"
        elif .name=="categories" then .value=$cats
        elif .name=="minimumSeeders" then .value=1
        else . end
      )')"
  code="$(curl -ksS -o "$BODY" -w '%{http_code}' -H "X-Api-Key: $key" -H 'Content-Type: application/json' -X POST -d "$payload" "$base/api/v3/indexer")"
  if [[ "$code" == "200" || "$code" == "201" ]]; then
    id="$(jq -r '.id' "$BODY")"
    mark "$label indexer" "PASS" "id=$id"
    printf '%s\n' "$id"
  else
    mark "$label indexer" "FAIL" "HTTP $code $(tr '\n' ' ' <"$BODY")"
    printf '\n'
  fi
}

wait_for_indexer() {
  local label="$1"
  local url="$2"
  for _ in $(seq 1 30); do
    if docker run --rm --network "$NETWORK" alpine:3.20 wget -qO /dev/null "$url/api?t=caps&apikey=fixture" >/dev/null 2>&1; then
      mark "$label fixture indexer HTTP" "PASS" "$url"
      return 0
    fi
    sleep 1
  done
  mark "$label fixture indexer HTTP" "FAIL" "$url not reachable"
  return 1
}

wait_for_torrent() {
  local name="$1"
  local deadline=$((SECONDS + 150))
  while (( SECONDS < deadline )); do
    row="$(curl -ksS -b "$COOKIE_JAR" "$TNG_HOST_URL/api/qb/v2/torrents/info" \
      | jq -c --arg name "$name" '.[] | select(.name==$name) | select((.progress // 0) >= 1)' | head -1)"
    if [[ -n "$row" ]]; then
      printf '%s\n' "$row"
      return 0
    fi
    sleep 2
  done
  return 1
}

grab_release() {
  local label="$1"
  local base="$2"
  local key="$3"
  local torrent_name="$4"
  local release="$5"
  local code row

  code="$(curl -ksS -o "$BODY" -w '%{http_code}' -H "X-Api-Key: $key" -H 'Content-Type: application/json' -X POST -d "$release" "$base/api/v3/release")"
  if [[ "$code" == "200" || "$code" == "201" || "$code" == "202" ]]; then
    mark "$label release grab" "PASS" "submitted to qBittorrent-compatible TorrentNG client"
  else
    mark "$label release grab" "FAIL" "HTTP $code $(tr '\n' ' ' <"$BODY")"
    return
  fi

  if row="$(wait_for_torrent "$torrent_name")"; then
    mark "$label fixture transfer" "PASS" "$(jq -r '"progress=\(.progress) size=\(.size) downloaded=\(.downloaded)"' <<<"$row")"
  else
    mark "$label fixture transfer" "FAIL" "$torrent_name did not complete within timeout"
  fi
}

{
  echo "# TorrentNG Sonarr/Radarr App Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Scope: app indexer test, app search/cache, torrent URL availability, and optional app release grab/transfer"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

SONARR_BASE="$(mapped_host_url "$SONARR_CONTAINER" 8989)"
RADARR_BASE="$(mapped_host_url "$RADARR_CONTAINER" 7878)"
TNG_API_TOKEN="${TNG_API_TOKEN:-cert-token}"
TNG_HOST_URL="${TNG_HOST_URL:-http://localhost:${TNG_HOST_PORT:-18080}}"
TNG_HOST_URL="$(mapped_host_url "$TNG_CONTAINER" 8080)"
SONARR_KEY="${SONARR_API_KEY_OVERRIDE:-$(api_key_from_container "$SONARR_CONTAINER")}"
RADARR_KEY="${RADARR_API_KEY_OVERRIDE:-$(api_key_from_container "$RADARR_CONTAINER")}"

docker run -d --rm --name "$TRACKER_NAME" --network "$NETWORK" lednerb/opentracker-docker >/dev/null
docker run --rm --network "$NETWORK" -v "$DOWNLOADS_VOLUME:/downloads" alpine:3.20 sh -lc "
  apk add --no-cache mktorrent >/dev/null
  rm -rf /downloads/cert-arr-fixture
  mkdir -p /downloads/cert-arr-fixture/sonarr-seed /downloads/cert-arr-fixture/radarr-seed
  dd if=/dev/urandom of=/downloads/cert-arr-fixture/sonarr-seed/tng-sonarr-fixture.bin bs=$FIXTURE_BYTES count=1 status=none
  dd if=/dev/urandom of=/downloads/cert-arr-fixture/radarr-seed/tng-radarr-fixture.bin bs=$FIXTURE_BYTES count=1 status=none
  mktorrent -a http://$TRACKER_NAME:6969/announce -o /downloads/cert-arr-fixture/tng-sonarr-fixture.torrent /downloads/cert-arr-fixture/sonarr-seed/tng-sonarr-fixture.bin >/dev/null
  mktorrent -a http://$TRACKER_NAME:6969/announce -o /downloads/cert-arr-fixture/tng-radarr-fixture.torrent /downloads/cert-arr-fixture/radarr-seed/tng-radarr-fixture.bin >/dev/null
"
docker run -d --rm --name "$SONARR_SEEDER_NAME" --network "$NETWORK" -v "$DOWNLOADS_VOLUME:/downloads" \
  alpine:3.20 sh -lc 'apk add --no-cache transmission-cli >/dev/null && exec transmission-cli -w /downloads/cert-arr-fixture/sonarr-seed /downloads/cert-arr-fixture/tng-sonarr-fixture.torrent' >/dev/null
docker run -d --rm --name "$RADARR_SEEDER_NAME" --network "$NETWORK" -v "$DOWNLOADS_VOLUME:/downloads" \
  alpine:3.20 sh -lc 'apk add --no-cache transmission-cli >/dev/null && exec transmission-cli -w /downloads/cert-arr-fixture/radarr-seed /downloads/cert-arr-fixture/tng-radarr-fixture.torrent' >/dev/null
mark "fixture torrents" "PASS" "separate $FIXTURE_BYTES byte Sonarr/Radarr torrents and stock seeders ready"

docker run -d --rm --name "$SONARR_INDEXER_CONTAINER" --network "$NETWORK" \
  -v "$DOWNLOADS_VOLUME:/downloads" \
  -v "$ROOT/deploy/certification/fixture_indexer.py:/fixture_indexer.py:ro" \
  -e "FIXTURE_PUBLIC_BASE=http://$SONARR_INDEXER_CONTAINER:8082" \
  -e "FIXTURE_TORRENT_PATH=/downloads/cert-arr-fixture/tng-sonarr-fixture.torrent" \
  -e "FIXTURE_TITLE=Breaking.Bad.S01E01.1080p.WEB-DL-TNG" \
  -e "FIXTURE_GUID=$FIXTURE_ID-sonarr" \
  python:3-alpine python /fixture_indexer.py >/dev/null
docker run -d --rm --name "$RADARR_INDEXER_CONTAINER" --network "$NETWORK" \
  -v "$DOWNLOADS_VOLUME:/downloads" \
  -v "$ROOT/deploy/certification/fixture_indexer.py:/fixture_indexer.py:ro" \
  -e "FIXTURE_PUBLIC_BASE=http://$RADARR_INDEXER_CONTAINER:8082" \
  -e "FIXTURE_TORRENT_PATH=/downloads/cert-arr-fixture/tng-radarr-fixture.torrent" \
  -e "FIXTURE_TITLE=The.Matrix.1999.1080p.WEB-DL-TNG" \
  -e "FIXTURE_GUID=$FIXTURE_ID-radarr" \
  python:3-alpine python /fixture_indexer.py >/dev/null

wait_for_indexer "Sonarr" "http://$SONARR_INDEXER_CONTAINER:8082" || true
wait_for_indexer "Radarr" "http://$RADARR_INDEXER_CONTAINER:8082" || true

ensure_root_folder "Sonarr" "$SONARR_BASE" "$SONARR_KEY" "/tv" "$SONARR_CONTAINER"
ensure_root_folder "Radarr" "$RADARR_BASE" "$RADARR_KEY" "/movies" "$RADARR_CONTAINER"
ensure_sonarr_series "$SONARR_BASE" "$SONARR_KEY"
ensure_radarr_movie "$RADARR_BASE" "$RADARR_KEY"

sonarr_indexer_id="$(create_indexer "Sonarr" "$SONARR_BASE" "$SONARR_KEY" "$SONARR_INDEXER_NAME" "http://$SONARR_INDEXER_CONTAINER:8082" '[5030,5040]')"
radarr_indexer_id="$(create_indexer "Radarr" "$RADARR_BASE" "$RADARR_KEY" "$RADARR_INDEXER_NAME" "http://$RADARR_INDEXER_CONTAINER:8082" '[2040]')"
[[ -n "$sonarr_indexer_id" ]] || status="FAIL"
[[ -n "$radarr_indexer_id" ]] || status="FAIL"

if [[ "$ARR_GRAB" == "1" ]]; then
  code="$(curl -ksS -o "$BODY" -w '%{http_code}' "$TNG_HOST_URL/api/qb/v2/auth/login" -X POST -d "username=$TNG_API_TOKEN" -d "password=$TNG_API_TOKEN" -c "$COOKIE_JAR")"
  if [[ "$code" == "200" ]]; then
    mark "qBit auth" "PASS" "session cookie accepted for transfer verification"
  else
    mark "qBit auth" "FAIL" "HTTP $code"
  fi
fi

if [[ -n "$sonarr_indexer_id" ]]; then
  eid="$(curl -fsS -H "X-Api-Key: $SONARR_KEY" "$SONARR_BASE/api/v3/episode?seriesId=1" | jq -r '.[] | select(.seasonNumber==1 and .episodeNumber==1) | .id' | head -1)"
  code="$(curl -ksS -o "$BODY" -w '%{http_code}' -H "X-Api-Key: $SONARR_KEY" "$SONARR_BASE/api/v3/release?episodeId=$eid")"
  releases="$(jq 'length' "$BODY" 2>/dev/null || echo 0)"
  if [[ "$code" == "200" && "$releases" -gt 0 ]]; then
    mark "Sonarr fixture search" "PASS" "releases=$releases"
    if [[ "$ARR_GRAB" == "1" ]]; then
      release="$(jq -c '.[0]' "$BODY")"
      grab_release "Sonarr" "$SONARR_BASE" "$SONARR_KEY" "tng-sonarr-fixture.bin" "$release"
    fi
  else
    mark "Sonarr fixture search" "FAIL" "HTTP $code releases=$releases"
  fi
fi

if [[ -n "$radarr_indexer_id" ]]; then
  movie_id="$(curl -fsS -H "X-Api-Key: $RADARR_KEY" "$RADARR_BASE/api/v3/movie" | jq -r '.[] | select(.title=="The Matrix") | .id' | head -1)"
  code="$(curl -ksS -o "$BODY" -w '%{http_code}' -H "X-Api-Key: $RADARR_KEY" "$RADARR_BASE/api/v3/release?movieId=$movie_id")"
  releases="$(jq 'length' "$BODY" 2>/dev/null || echo 0)"
  if [[ "$code" == "200" && "$releases" -gt 0 ]]; then
    mark "Radarr fixture search" "PASS" "releases=$releases"
    if [[ "$ARR_GRAB" == "1" ]]; then
      release="$(jq -c '.[0]' "$BODY")"
      grab_release "Radarr" "$RADARR_BASE" "$RADARR_KEY" "tng-radarr-fixture.bin" "$release"
    fi
  else
    mark "Radarr fixture search" "FAIL" "HTTP $code releases=$releases"
  fi
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
