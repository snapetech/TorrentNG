#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/curl_policy.sh
source "$ROOT/scripts/curl_policy.sh"
ENV_FILE="${CERT_ENV_FILE:-$ROOT/deploy/certification/.env}"
OUT="${1:-$ROOT/certification/reports/live-transfer-$(date -u +%Y%m%dT%H%M%SZ).md}"

ENV_TNG_HOST_URL="${TNG_HOST_URL:-}"
if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
fi

TNG_HOST_URL="${ENV_TNG_HOST_URL:-${TNG_HOST_URL:-http://localhost:${TNG_HOST_PORT:-18080}}}"
TNG_API_TOKEN="${TNG_API_TOKEN:-local-cert-api-token-20260904}"
python3 "$ROOT/scripts/protected_target.py" "$TNG_HOST_URL"
COMPOSE_PROJECT="${CERT_COMPOSE_PROJECT:-certification}"
COMPOSE_NETWORK="${CERT_COMPOSE_NETWORK:-${COMPOSE_PROJECT}_default}"
DOWNLOADS_VOLUME="${CERT_DOWNLOADS_VOLUME:-${COMPOSE_PROJECT}_downloads}"
FIXTURE_ID_SEED="${CERT_FIXTURE_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
FIXTURE_BYTES="${CERT_FIXTURE_BYTES:-1048576}"
PUBLIC_TORRENT_URL="${PUBLIC_TORRENT_URL:-https://mirror.arizona.edu/debian-cd/current/amd64/bt-cd/debian-13.4.0-amd64-netinst.iso.torrent}"
PUBLIC_TRANSFER="${PUBLIC_TRANSFER:-0}"
TRANSFER_TIMEOUT_SECS="${CERT_TRANSFER_TIMEOUT_SECS:-120}"
PUBLIC_TIMEOUT_SECS="${CERT_PUBLIC_TRANSFER_TIMEOUT_SECS:-1800}"

if [[ ! "$FIXTURE_ID_SEED" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,31}$ || "$FIXTURE_ID_SEED" == *..* ]]; then
  echo "CERT_FIXTURE_ID must be 1-32 safe filename characters without '..'" >&2
  exit 2
fi
FIXTURE_ID="$FIXTURE_ID_SEED-$$"
if [[ ! "$FIXTURE_BYTES" =~ ^[0-9]{1,8}$ ]]; then
  echo "CERT_FIXTURE_BYTES must be an integer between 1 and 67108864" >&2
  exit 2
fi
FIXTURE_BYTES=$((10#$FIXTURE_BYTES))
if (( FIXTURE_BYTES < 1 || FIXTURE_BYTES > 67108864 )); then
  echo "CERT_FIXTURE_BYTES must be an integer between 1 and 67108864" >&2
  exit 2
fi
if [[ "$PUBLIC_TRANSFER" != "0" && "$PUBLIC_TRANSFER" != "1" ]]; then
  echo "PUBLIC_TRANSFER must be 0 or 1" >&2
  exit 2
fi
for timeout_name in TRANSFER_TIMEOUT_SECS PUBLIC_TIMEOUT_SECS; do
  timeout_value="${!timeout_name}"
  if [[ ! "$timeout_value" =~ ^[0-9]{1,5}$ ]]; then
    echo "$timeout_name must be an integer between 1 and 7200" >&2
    exit 2
  fi
  timeout_value=$((10#$timeout_value))
  if (( timeout_value < 1 || timeout_value > 7200 )); then
    echo "$timeout_name must be an integer between 1 and 7200" >&2
    exit 2
  fi
  printf -v "$timeout_name" '%s' "$timeout_value"
done

FIXTURE_DOWNLOAD_DIR="cert-fixture-$FIXTURE_ID"
LOCAL_CATEGORY="cert-local-fixture-$FIXTURE_ID"
PUBLIC_CATEGORY="cert-public-$FIXTURE_ID"

TRACKER_NAME="tng-cert-tracker-$FIXTURE_ID"
SEEDER_NAME="tng-cert-seeder-$FIXTURE_ID"
FILESERVER_NAME="tng-cert-files-$FIXTURE_ID"
COOKIE_JAR="$(mktemp "${TMPDIR:-/tmp}/tng-transfer-cookies.XXXXXX")"
BODY="$(mktemp "${TMPDIR:-/tmp}/tng-transfer-body.XXXXXX")"
AUTH_BODY_FILE="$(mktemp "${TMPDIR:-/tmp}/tng-transfer-auth.XXXXXX")"
FIXTURE_TORRENT_FILE="$(mktemp "${TMPDIR:-/tmp}/tng-fixture-torrent.XXXXXX")"

mkdir -p "$(dirname "$OUT")"

cleanup() {
  if [[ -s "$COOKIE_JAR" ]]; then
    delete_category_torrents "$LOCAL_CATEGORY" || true
    delete_category_torrents "$PUBLIC_CATEGORY" || true
  fi
  docker rm -f "$SEEDER_NAME" "$FILESERVER_NAME" "$TRACKER_NAME" >/dev/null 2>&1 || true
  docker run --rm -v "$DOWNLOADS_VOLUME:/downloads" alpine:3.20 \
    sh -c "rm -rf -- /downloads/$FIXTURE_DOWNLOAD_DIR" >/dev/null 2>&1 || true
  rm -f -- "$COOKIE_JAR" "$BODY" "$AUTH_BODY_FILE" "$FIXTURE_TORRENT_FILE"
}
trap cleanup EXIT
tng_write_qbit_login_body "$TNG_API_TOKEN" "$AUTH_BODY_FILE"

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
  curl -q -sS --noproxy "*" -o "$BODY" -w '%{http_code}' "$@" "$url" || true
}

wait_for_torrent() {
  local name="$1"
  local want_complete="$2"
  local timeout="${3:-$TRANSFER_TIMEOUT_SECS}"
  local deadline=$((SECONDS + timeout))
  while (( SECONDS < deadline )); do
    local row
    row="$(curl -q -sS --noproxy "*" -b "$COOKIE_JAR" "$TNG_HOST_URL/api/qb/v2/torrents/info" \
      | jq -c --arg name "$name" '.[] | select(.name==$name)' | head -1 || true)"
    if [[ -n "$row" ]]; then
      if [[ "$want_complete" != "1" || "$(jq -r '.progress >= 1' <<<"$row")" == "true" ]]; then
        printf '%s' "$row"
        return 0
      fi
    fi
    sleep 2
  done
  return 1
}

wait_for_category_torrent() {
  local category="$1"
  local want_complete="$2"
  local timeout="${3:-$TRANSFER_TIMEOUT_SECS}"
  local deadline=$((SECONDS + timeout))
  while (( SECONDS < deadline )); do
    local row
    row="$(curl -q -sS --noproxy "*" -b "$COOKIE_JAR" "$TNG_HOST_URL/api/qb/v2/torrents/info?category=$category" \
      | jq -c 'sort_by(.progress) | reverse | .[0] // empty' || true)"
    if [[ -n "$row" ]]; then
      if [[ "$want_complete" != "1" || "$(jq -r '.progress >= 1' <<<"$row")" == "true" ]]; then
        printf '%s' "$row"
        return 0
      fi
    fi
    sleep 5
  done
  return 1
}

delete_category_torrents() {
  local category="$1"
  local hashes
  hashes="$(curl -q -sS --noproxy "*" -b "$COOKIE_JAR" "$TNG_HOST_URL/api/qb/v2/torrents/info?category=$category" \
    | jq -r '.[].hash' | paste -sd '|' -)"
  if [[ -n "$hashes" ]]; then
    curl -q -sS --noproxy "*" -b "$COOKIE_JAR" -X POST \
      -d "hashes=$hashes" \
      -d "deleteFiles=true" \
      "$TNG_HOST_URL/api/qb/v2/torrents/delete" >/dev/null || true
    sleep 2
  fi
}

{
  echo "# TorrentNG Live Transfer Certification Report"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- TorrentNG URL: $TNG_HOST_URL"
  echo "- Docker network: $COMPOSE_NETWORK"
  echo "- Downloads volume: $DOWNLOADS_VOLUME"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

code="$(http_code "$TNG_HOST_URL/api/qb/v2/auth/login" -X POST --data-binary "@$AUTH_BODY_FILE" -c "$COOKIE_JAR")"
if [[ "$code" == "200" && "$(cat "$BODY")" == "Ok." ]]; then
  mark "qBit auth" "PASS" "session cookie accepted"
else
  mark "qBit auth" "FAIL" "HTTP $code body=$(tr '\n' ' ' <"$BODY")"
  echo >> "$OUT"
  echo "Overall status: $status" >> "$OUT"
  echo "$OUT"
  exit 1
fi

delete_category_torrents "$LOCAL_CATEGORY"
delete_category_torrents "$PUBLIC_CATEGORY"

docker run -d --rm --name "$TRACKER_NAME" --network "$COMPOSE_NETWORK" alpine:3.20 \
  sh -lc 'apk add --no-cache opentracker >/dev/null && exec opentracker -i 0.0.0.0 -p 6969 -P 6969' >/dev/null
sleep 2
mark "local tracker" "PASS" "$TRACKER_NAME listening on http://$TRACKER_NAME:6969/announce"

docker run --rm --network "$COMPOSE_NETWORK" -v "$DOWNLOADS_VOLUME:/downloads" alpine:3.20 sh -lc "
  set -e
  apk add --no-cache mktorrent >/dev/null
  mkdir -p /downloads/$FIXTURE_DOWNLOAD_DIR/seed /downloads/$FIXTURE_DOWNLOAD_DIR/leech
  dd if=/dev/urandom of=/downloads/$FIXTURE_DOWNLOAD_DIR/seed/tng-fixture.bin bs=$FIXTURE_BYTES count=1 status=none
  mktorrent -a http://$TRACKER_NAME:6969/announce -o /downloads/$FIXTURE_DOWNLOAD_DIR/tng-fixture.torrent /downloads/$FIXTURE_DOWNLOAD_DIR/seed/tng-fixture.bin >/dev/null
"
docker run --rm -v "$DOWNLOADS_VOLUME:/downloads:ro" alpine:3.20 \
  cat "/downloads/$FIXTURE_DOWNLOAD_DIR/tng-fixture.torrent" > "$FIXTURE_TORRENT_FILE"
mark "fixture torrent" "PASS" "$FIXTURE_BYTES byte torrent generated in Docker volume"

docker run -d --rm --name "$FILESERVER_NAME" --network "$COMPOSE_NETWORK" -v "$DOWNLOADS_VOLUME:/downloads:ro" alpine:3.20 \
  sh -lc "apk add --no-cache busybox-extras >/dev/null && exec httpd -f -p 8081 -h /downloads/$FIXTURE_DOWNLOAD_DIR" >/dev/null
sleep 1
if docker run --rm --network "$COMPOSE_NETWORK" alpine:3.20 \
  wget -qO /dev/null "http://$FILESERVER_NAME:8081/tng-fixture.torrent"; then
  mark "fixture torrent HTTP" "PASS" "http://$FILESERVER_NAME:8081/tng-fixture.torrent"
else
  mark "fixture torrent HTTP" "FAIL" "file server not reachable"
fi

docker run -d --rm --name "$SEEDER_NAME" --network "$COMPOSE_NETWORK" -v "$DOWNLOADS_VOLUME:/downloads" alpine:3.20 \
  sh -lc "apk add --no-cache transmission-cli >/dev/null && exec transmission-cli -w /downloads/$FIXTURE_DOWNLOAD_DIR/seed /downloads/$FIXTURE_DOWNLOAD_DIR/tng-fixture.torrent" >/dev/null
sleep 4
mark "stock seeder" "PASS" "$SEEDER_NAME running transmission-cli"

code="$(curl -q -sS --noproxy "*" -o "$BODY" -w '%{http_code}' -b "$COOKIE_JAR" \
  -F "urls=http://$FILESERVER_NAME:8081/tng-fixture.torrent" \
  -F "savepath=/data/$FIXTURE_DOWNLOAD_DIR/leech" \
  -F "category=$LOCAL_CATEGORY" \
  -F "stopped=false" \
  "$TNG_HOST_URL/api/qb/v2/torrents/add" || true)"
if [[ "$code" == "200" && "$(cat "$BODY")" == "Ok." ]]; then
  mark "TorrentNG add local fixture URL" "PASS" "qBit add accepted torrent URL"
else
  mark "TorrentNG add local fixture URL" "FAIL" "HTTP $code body=$(tr '\n' ' ' <"$BODY")"
fi

if row="$(wait_for_torrent "tng-fixture.bin" 1)"; then
  mark "local fixture transfer" "PASS" "$(jq -r '"progress=\(.progress) size=\(.size) downloaded=\(.downloaded)"' <<<"$row")"
elif docker run --rm -v "$DOWNLOADS_VOLUME:/downloads:ro" alpine:3.20 \
  sh -lc "test \"\$(wc -c </downloads/$FIXTURE_DOWNLOAD_DIR/leech/tng-fixture.bin 2>/dev/null || echo 0)\" -eq $FIXTURE_BYTES"; then
  mark "local fixture transfer" "PASS" "completed on disk; qBit cache did not report before timeout"
else
  mark "local fixture transfer" "FAIL" "fixture did not complete within timeout"
fi

code="$(curl -q -sS --noproxy "*" -o "$BODY" -w '%{http_code}' -b "$COOKIE_JAR" \
  -F "urls=$PUBLIC_TORRENT_URL" \
  -F "savepath=/data/$FIXTURE_DOWNLOAD_DIR/public" \
  -F "category=$PUBLIC_CATEGORY" \
  -F "stopped=$([[ "$PUBLIC_TRANSFER" == "1" ]] && echo false || echo true)" \
  "$TNG_HOST_URL/api/qb/v2/torrents/add" || true)"
if [[ "$code" == "200" && "$(cat "$BODY")" == "Ok." ]]; then
  mark "public Linux torrent add" "PASS" "$PUBLIC_TORRENT_URL"
else
  mark "public Linux torrent add" "FAIL" "HTTP $code body=$(tr '\n' ' ' <"$BODY")"
fi

if [[ "$PUBLIC_TRANSFER" == "1" ]]; then
  if row="$(wait_for_category_torrent "$PUBLIC_CATEGORY" 1 "$PUBLIC_TIMEOUT_SECS")"; then
    mark "public Linux transfer" "PASS" "$(jq -r '"progress=\(.progress) size=\(.size) downloaded=\(.downloaded)"' <<<"$row")"
  else
    row="$(curl -q -sS --noproxy "*" -b "$COOKIE_JAR" "$TNG_HOST_URL/api/qb/v2/torrents/info?category=$PUBLIC_CATEGORY" \
      | jq -c 'sort_by(.progress) | reverse | .[0] // empty' || true)"
    mark "public Linux transfer" "FAIL" "did not complete within ${PUBLIC_TIMEOUT_SECS}s latest=${row:-none}"
  fi
else
  mark "public Linux transfer" "INFO" "skipped by default; set PUBLIC_TRANSFER=1 to download"
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
