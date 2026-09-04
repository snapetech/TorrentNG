#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${CERT_ENV_FILE:-$ROOT/deploy/certification/.env}"
COMPOSE_PROJECT="${CERT_COMPOSE_PROJECT:-certification}"
COMPOSE_NETWORK="${CERT_COMPOSE_NETWORK:-${COMPOSE_PROJECT}_default}"
DOWNLOADS_VOLUME="${CERT_DOWNLOADS_VOLUME:-${COMPOSE_PROJECT}_downloads}"
OUT="${1:-$ROOT/certification/reports/transfer-churn-$(date -u +%Y%m%dT%H%M%SZ).md}"

ENV_TNG_HOST_URL="${TNG_HOST_URL:-}"
if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
fi

TNG_HOST_URL="${ENV_TNG_HOST_URL:-${TNG_HOST_URL:-http://localhost:${TNG_HOST_PORT:-18080}}}"
TNG_API_TOKEN="${TNG_API_TOKEN:-local-cert-api-token-20260904}"
TNG_CONTAINER="${TNG_CONTAINER:-certification-torrentng-1}"
CHURN_CYCLES="${TRANSFER_CHURN_CYCLES:-5}"
FIXTURE_BYTES="${TRANSFER_CHURN_FIXTURE_BYTES:-16777216}"
TRANSFER_TIMEOUT_SECS="${TRANSFER_CHURN_TIMEOUT_SECS:-180}"
MAX_RSS_MB="${TRANSFER_CHURN_MAX_RSS_MB:-500}"
PUBLIC_TORRENT_URL="${PUBLIC_TORRENT_URL:-https://mirror.arizona.edu/debian-cd/current/amd64/bt-cd/debian-13.4.0-amd64-netinst.iso.torrent}"
PUBLIC_CYCLES="${TRANSFER_CHURN_PUBLIC_CYCLES:-0}"
FIXTURE_ID="${TRANSFER_CHURN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
CATEGORY="cert-transfer-churn"
TRACKER_NAME="tng-churn-tracker-$FIXTURE_ID"
FILESERVER_NAME="tng-churn-files-$FIXTURE_ID"
COOKIE_JAR="/tmp/tng-churn-cookies-$FIXTURE_ID.txt"
BODY="/tmp/tng-churn-body-$FIXTURE_ID.txt"

mkdir -p "$(dirname "$OUT")"
: > "$BODY"

mapped="$(docker port "$TNG_CONTAINER" 8080/tcp 2>/dev/null | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -1 || true)"
if [[ -n "$mapped" && "$TNG_HOST_URL" == http://localhost:* ]]; then
  TNG_HOST_URL="http://localhost:$mapped"
fi

status="PASS"
seeders=()

mark() {
  local name="$1"
  local result="$2"
  local detail="$3"
  detail="${detail//$'\n'/ }"
  detail="${detail//|/\\|}"
  printf '| %s | %s | %s |\n' "$name" "$result" "$detail" >> "$OUT"
  [[ "$result" == "FAIL" ]] && status="FAIL"
  return 0
}

cleanup() {
  if ((${#seeders[@]} > 0)); then
    for seeder in "${seeders[@]}"; do
      docker rm -f "$seeder" >/dev/null 2>&1 || true
    done
  fi
  docker rm -f "$FILESERVER_NAME" "$TRACKER_NAME" >/dev/null 2>&1 || true
  rm -f "$COOKIE_JAR" "$BODY"
}
trap cleanup EXIT

http_code() {
  local url="$1"
  shift || true
  curl -ksS -o "$BODY" -w '%{http_code}' "$@" "$url" || true
}

rss_mb() {
  docker exec "$TNG_CONTAINER" sh -lc "awk '/VmRSS:/ {printf \"%.1f\", \$2 / 1024}' /proc/1/status" 2>/dev/null || echo 0
}

delete_category_torrents() {
  local hashes
  hashes="$(curl -ksS -b "$COOKIE_JAR" "$TNG_HOST_URL/api/qb/v2/torrents/info?category=$CATEGORY" \
    | jq -r '.[].hash' | paste -sd '|' -)"
  if [[ -n "$hashes" ]]; then
    curl -ksS -b "$COOKIE_JAR" -X POST \
      -d "hashes=$hashes" \
      -d "deleteFiles=true" \
      "$TNG_HOST_URL/api/qb/v2/torrents/delete" >/dev/null || true
  fi
  return 0
}

read_disk_bytes() {
  local path="$1"
  docker run --rm -v "$DOWNLOADS_VOLUME:/downloads:ro" alpine:3.20 \
    sh -lc "wc -c < '$path' 2>/dev/null || echo 0" | tr -d '[:space:]' || echo 0
}

wait_for_completion() {
  local name="$1"
  local path="$2"
  local bytes="$3"
  local deadline=$((SECONDS + TRANSFER_TIMEOUT_SECS))
  while (( SECONDS < deadline )); do
    local row
    row="$(curl -ksS -b "$COOKIE_JAR" "$TNG_HOST_URL/api/qb/v2/torrents/info?category=$CATEGORY" \
      | jq -c --arg name "$name" '.[] | select(.name==$name)' | head -1 || true)"
    if [[ -n "$row" && "$(jq -r '.progress >= 1' <<<"$row")" == "true" ]]; then
      printf '%s' "$row"
      return 0
    fi
    local observed
    observed="$(read_disk_bytes "$path")"
    if [[ "$observed" == "$bytes" ]]; then
      printf '{"progress":1,"downloaded":%s}' "$observed"
      return 0
    fi
    sleep 2
  done
  return 1
}

{
  echo "# TorrentNG Transfer Churn Soak"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- TorrentNG URL: $TNG_HOST_URL"
  echo "- Docker network: $COMPOSE_NETWORK"
  echo "- Downloads volume: $DOWNLOADS_VOLUME"
  echo "- Local cycles: $CHURN_CYCLES"
  echo "- Fixture bytes per cycle: $FIXTURE_BYTES"
  echo "- Public Linux torrent cycles: $PUBLIC_CYCLES"
  [[ "$PUBLIC_CYCLES" != "0" ]] && echo "- Public torrent URL: $PUBLIC_TORRENT_URL"
  echo "- Transfer timeout seconds: $TRANSFER_TIMEOUT_SECS"
  echo "- Max RSS MB: $MAX_RSS_MB"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

code="$(http_code "$TNG_HOST_URL/api/qb/v2/auth/login" -X POST -d "username=$TNG_API_TOKEN" -d "password=$TNG_API_TOKEN" -c "$COOKIE_JAR")"
if [[ "$code" == "200" && "$(cat "$BODY")" == "Ok." ]]; then
  mark "qBit auth" "PASS" "session cookie accepted"
else
  mark "qBit auth" "FAIL" "HTTP $code body=$(tr '\n' ' ' <"$BODY")"
  echo >> "$OUT"; echo "Overall status: $status" >> "$OUT"; echo "$OUT"; exit 1
fi

delete_category_torrents

docker run -d --rm --name "$TRACKER_NAME" --network "$COMPOSE_NETWORK" alpine:3.20 \
  sh -lc 'apk add --no-cache opentracker >/dev/null && exec opentracker -i 0.0.0.0 -p 6969 -P 6969' >/dev/null
sleep 2
mark "local tracker" "PASS" "http://$TRACKER_NAME:6969/announce"

docker run -d --rm --name "$FILESERVER_NAME" --network "$COMPOSE_NETWORK" -v "$DOWNLOADS_VOLUME:/downloads:ro" alpine:3.20 \
  sh -lc 'apk add --no-cache busybox-extras >/dev/null && exec httpd -f -p 8081 -h /downloads/transfer-churn' >/dev/null
sleep 1
mark "fixture file server" "PASS" "http://$FILESERVER_NAME:8081/"

{
  echo
  echo "## Transfer Cycles"
  echo
  echo "| Cycle | Type | Add HTTP | Progress | Downloaded | RSS MB | Result |"
  echo "|---:|---|---:|---:|---:|---:|---|"
} >> "$OUT"

max_rss="0"
for cycle in $(seq 1 "$CHURN_CYCLES"); do
  name="tng-churn-$FIXTURE_ID-$cycle.bin"
  torrent="tng-churn-$FIXTURE_ID-$cycle.torrent"
  seeder="tng-churn-seeder-$FIXTURE_ID-$cycle"
  seeders+=("$seeder")

  docker run --rm --network "$COMPOSE_NETWORK" -v "$DOWNLOADS_VOLUME:/downloads" alpine:3.20 sh -lc "
    set -e
    apk add --no-cache mktorrent >/dev/null
    rm -rf /downloads/transfer-churn/$cycle
    mkdir -p /downloads/transfer-churn/$cycle/seed /downloads/transfer-churn/$cycle/leech
    dd if=/dev/urandom of=/downloads/transfer-churn/$cycle/seed/$name bs=$FIXTURE_BYTES count=1 status=none
    mktorrent -a http://$TRACKER_NAME:6969/announce -o /downloads/transfer-churn/$cycle/$torrent /downloads/transfer-churn/$cycle/seed/$name >/dev/null
  "

  docker run -d --rm --name "$seeder" --network "$COMPOSE_NETWORK" -v "$DOWNLOADS_VOLUME:/downloads" alpine:3.20 \
    sh -lc "apk add --no-cache transmission-cli >/dev/null && exec transmission-cli -w /downloads/transfer-churn/$cycle/seed /downloads/transfer-churn/$cycle/$torrent" >/dev/null
  sleep 3

  code="$(curl -ksS -o "$BODY" -w '%{http_code}' -b "$COOKIE_JAR" \
    -F "urls=http://$FILESERVER_NAME:8081/$cycle/$torrent" \
    -F "savepath=/data/transfer-churn/$cycle/leech" \
    -F "category=$CATEGORY" \
    -F "stopped=false" \
    "$TNG_HOST_URL/api/qb/v2/torrents/add" || true)"

  progress="0"
  downloaded="0"
  result="FAIL"
  if [[ "$code" == "200" && "$(cat "$BODY")" == "Ok." ]] && row="$(wait_for_completion "$name" "/downloads/transfer-churn/$cycle/leech/$name" "$FIXTURE_BYTES")"; then
    progress="$(jq -r '.progress' <<<"$row")"
    downloaded="$(jq -r '.downloaded' <<<"$row")"
    result="PASS"
  fi

  rss="$(rss_mb)"
  awk -v a="$rss" -v b="$max_rss" 'BEGIN {exit !(a > b)}' && max_rss="$rss"
  printf '| %s | local fixture | %s | %s | %s | %s | %s |\n' "$cycle" "$code" "$progress" "$downloaded" "$rss" "$result" >> "$OUT"
  [[ "$result" == "PASS" ]] || status="FAIL"

  delete_category_torrents
  docker rm -f "$seeder" >/dev/null 2>&1 || true
done

for cycle in $(seq 1 "$PUBLIC_CYCLES"); do
  code="$(curl -ksS -o "$BODY" -w '%{http_code}' -b "$COOKIE_JAR" \
    -F "urls=$PUBLIC_TORRENT_URL" \
    -F "savepath=/data/transfer-churn-public/$cycle" \
    -F "category=$CATEGORY" \
    -F "stopped=false" \
    "$TNG_HOST_URL/api/qb/v2/torrents/add" || true)"
  sleep 10
  row="$(curl -ksS -b "$COOKIE_JAR" "$TNG_HOST_URL/api/qb/v2/torrents/info?category=$CATEGORY" | jq -c '.[0] // empty' || true)"
  progress="$(jq -r '.progress // 0' <<<"${row:-{}}")"
  downloaded="$(jq -r '.downloaded // 0' <<<"${row:-{}}")"
  rss="$(rss_mb)"
  awk -v a="$rss" -v b="$max_rss" 'BEGIN {exit !(a > b)}' && max_rss="$rss"
  result="$([[ "$code" == "200" && -n "$row" ]] && echo PASS || echo FAIL)"
  printf '| %s | public Linux torrent | %s | %s | %s | %s | %s |\n' "$cycle" "$code" "$progress" "$downloaded" "$rss" "$result" >> "$OUT"
  [[ "$result" == "PASS" ]] || status="FAIL"
  delete_category_torrents
done

if awk -v rss="$max_rss" -v limit="$MAX_RSS_MB" 'BEGIN {exit !(rss <= limit)}'; then
  mark "memory ceiling" "PASS" "max RSS ${max_rss}MB <= ${MAX_RSS_MB}MB"
else
  mark "memory ceiling" "FAIL" "max RSS ${max_rss}MB > ${MAX_RSS_MB}MB"
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
