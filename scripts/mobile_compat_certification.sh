#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/mobile-compat-$(date -u +%Y%m%dT%H%M%SZ).md}"
TNG_HOST_URL="${TNG_HOST_URL:-http://localhost:${TNG_HOST_PORT:-18080}}"
TNG_API_TOKEN="${TNG_API_TOKEN:-local-cert-api-token-20260904}"
TNG_CONTAINER="${TNG_CONTAINER:-certification-torrentng-1}"
MOBILE_LIST_LIMIT="${MOBILE_LIST_LIMIT:-50000}"
BODY="$(mktemp)"

mkdir -p "$(dirname "$OUT")"

mapped="$(docker port "$TNG_CONTAINER" 8080/tcp 2>/dev/null | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -1 || true)"
if [[ -n "$mapped" && "$TNG_HOST_URL" == http://localhost:* ]]; then
  TNG_HOST_URL="http://localhost:$mapped"
fi

cleanup() {
  rm -f "$BODY" /tmp/tng-mobile-*.cookies
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

http_code() {
  local url="$1"
  shift || true
  curl -ksS -o "$BODY" -w '%{http_code}' "$@" "$url" || true
}

check_json_array_len() {
  jq 'length' "$BODY" 2>/dev/null || echo 0
}

run_profile() {
  local label="$1"
  local prefix="$2"
  local user_agent="$3"
  local cookie="/tmp/tng-mobile-${label//[^A-Za-z0-9]/-}.cookies"
  local code len hash rid delta_len

  code="$(http_code "$TNG_HOST_URL$prefix/auth/login" -A "$user_agent" -X POST -d "username=$TNG_API_TOKEN" -d "password=$TNG_API_TOKEN" -c "$cookie")"
  if [[ "$code" == "200" && "$(cat "$BODY")" == "Ok." ]]; then
    mark "$label login" "PASS" "$prefix auth cookie accepted"
  else
    mark "$label login" "FAIL" "HTTP $code body=$(tr '\n' ' ' <"$BODY")"
    return
  fi

  for endpoint in \
    "/app/version" \
    "/app/webapiVersion" \
    "/app/buildInfo" \
    "/app/preferences" \
    "/app/defaultSavePath" \
    "/transfer/info" \
    "/torrents/categories" \
    "/torrents/tags"; do
    code="$(http_code "$TNG_HOST_URL$prefix$endpoint" -A "$user_agent" -b "$cookie")"
    if [[ "$code" == "200" ]]; then
      mark "$label $endpoint" "PASS" "HTTP 200"
    else
      mark "$label $endpoint" "FAIL" "HTTP $code"
    fi
  done

  code="$(http_code "$TNG_HOST_URL$prefix/torrents/info?limit=$MOBILE_LIST_LIMIT&sort=name" -A "$user_agent" -b "$cookie")"
  len="$(check_json_array_len)"
  hash="$(jq -r '.[0].hash // empty' "$BODY" 2>/dev/null || true)"
  if [[ "$code" == "200" && "$len" -gt 0 && -n "$hash" ]]; then
    mark "$label list" "PASS" "HTTP 200 rows=$len first_hash=$hash"
  else
    mark "$label list" "FAIL" "HTTP $code rows=$len"
    return
  fi

  code="$(http_code "$TNG_HOST_URL$prefix/torrents/properties?hash=$hash" -A "$user_agent" -b "$cookie")"
  if [[ "$code" == "200" && "$(jq -r '.total_size // empty' "$BODY" 2>/dev/null)" =~ ^[0-9]+$ ]]; then
    mark "$label properties" "PASS" "$hash total_size=$(jq -r '.total_size' "$BODY")"
  else
    mark "$label properties" "FAIL" "HTTP $code body=$(tr '\n' ' ' <"$BODY")"
  fi

  for query in \
    "filter=completed&limit=25" \
    "filter=paused&limit=25" \
    "category=cert-scale&limit=25" \
    "sort=ratio&reverse=true&limit=25"; do
    code="$(http_code "$TNG_HOST_URL$prefix/torrents/info?$query" -A "$user_agent" -b "$cookie")"
    len="$(check_json_array_len)"
    if [[ "$code" == "200" ]]; then
      mark "$label filter $query" "PASS" "rows=$len"
    else
      mark "$label filter $query" "FAIL" "HTTP $code"
    fi
  done

  code="$(http_code "$TNG_HOST_URL$prefix/sync/maindata?rid=0" -A "$user_agent" -b "$cookie")"
  rid="$(jq -r '.rid // empty' "$BODY" 2>/dev/null || true)"
  len="$(jq '.torrents | length' "$BODY" 2>/dev/null || echo 0)"
  if [[ "$code" == "200" && -n "$rid" && "$len" -gt 0 ]]; then
    mark "$label sync full" "PASS" "rid=$rid torrents=$len"
  else
    mark "$label sync full" "FAIL" "HTTP $code rid=$rid torrents=$len"
    return
  fi

  code="$(http_code "$TNG_HOST_URL$prefix/sync/maindata?rid=$rid" -A "$user_agent" -b "$cookie")"
  delta_len="$(jq '.torrents | length' "$BODY" 2>/dev/null || echo 0)"
  if [[ "$code" == "200" ]]; then
    mark "$label sync delta" "PASS" "HTTP 200 torrents=$delta_len"
  else
    mark "$label sync delta" "FAIL" "HTTP $code"
  fi
}

{
  echo "# TorrentNG Mobile Compatibility Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- TorrentNG URL: $TNG_HOST_URL"
  echo "- List limit: $MOBILE_LIST_LIMIT"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

run_profile "NZB360-style /api/qb/v2" "/api/qb/v2" "NZB360/20 qBittorrent"
run_profile "Transdrone-style /api/v2" "/api/v2" "Transdrone Android qBittorrent"

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
