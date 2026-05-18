#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/soak-$(date -u +%Y%m%dT%H%M%SZ).md}"
TNG_HOST_URL="${TNG_HOST_URL:-http://localhost:${TNG_HOST_PORT:-18080}}"
TNG_API_TOKEN="${TNG_API_TOKEN:-cert-token}"
TNG_CONTAINER="${TNG_CONTAINER:-certification-torrentng-1}"
SOAK_DURATION_SECONDS="${SOAK_DURATION_SECONDS:-86400}"
SOAK_INTERVAL_SECONDS="${SOAK_INTERVAL_SECONDS:-60}"
SOAK_MAX_RSS_MB="${SOAK_MAX_RSS_MB:-500}"
SOAK_LIST_LIMIT="${SOAK_LIST_LIMIT:-50000}"
COOKIE_JAR="$(mktemp)"
BODY="$(mktemp)"

mkdir -p "$(dirname "$OUT")"

mapped="$(docker port "$TNG_CONTAINER" 8080/tcp 2>/dev/null | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -1 || true)"
if [[ -n "$mapped" && "$TNG_HOST_URL" == http://localhost:* ]]; then
  TNG_HOST_URL="http://localhost:$mapped"
fi

cleanup() {
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

rss_mb() {
  docker exec "$TNG_CONTAINER" sh -lc "awk '/VmRSS:/ {printf \"%.1f\", \$2 / 1024}' /proc/1/status"
}

{
  echo "# TorrentNG Soak Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- TorrentNG URL: $TNG_HOST_URL"
  echo "- Duration seconds: $SOAK_DURATION_SECONDS"
  echo "- Interval seconds: $SOAK_INTERVAL_SECONDS"
  echo "- Max RSS MB: $SOAK_MAX_RSS_MB"
  echo "- List limit: $SOAK_LIST_LIMIT"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

code="$(curl -ksS -o "$BODY" -w '%{http_code}' "$TNG_HOST_URL/api/qb/v2/auth/login" -X POST -d "username=$TNG_API_TOKEN" -d "password=$TNG_API_TOKEN" -c "$COOKIE_JAR")"
if [[ "$code" == "200" ]]; then
  mark "qBit auth" "PASS" "session cookie accepted"
else
  mark "qBit auth" "FAIL" "HTTP $code"
  echo >> "$OUT"; echo "Overall status: $status" >> "$OUT"; echo "$OUT"; exit 1
fi

{
  echo
  echo "## Samples"
  echo
  echo "| UTC | Health | Torrents | RSS MB | sync/maindata HTTP |"
  echo "|---|---:|---:|---:|---:|"
} >> "$OUT"

deadline=$((SECONDS + SOAK_DURATION_SECONDS))
samples=0
max_rss="0"
while (( SECONDS < deadline || samples == 0 )); do
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  health="$(curl -ksS -o "$BODY" -w '%{http_code}' "$TNG_HOST_URL/health" || true)"
  torrents="$(curl -ksS -b "$COOKIE_JAR" "$TNG_HOST_URL/api/qb/v2/torrents/info?limit=$SOAK_LIST_LIMIT" | jq 'length' 2>/dev/null || echo 0)"
  sync_code="$(curl -ksS -o "$BODY" -w '%{http_code}' -b "$COOKIE_JAR" "$TNG_HOST_URL/api/qb/v2/sync/maindata?rid=0" || true)"
  rss="$(rss_mb)"
  if awk -v a="$rss" -v b="$max_rss" 'BEGIN {exit !(a > b)}'; then
    max_rss="$rss"
  fi
  printf '| %s | %s | %s | %s | %s |\n' "$now" "$health" "$torrents" "$rss" "$sync_code" >> "$OUT"
  samples=$((samples + 1))
  if (( SECONDS >= deadline )); then
    break
  fi
  sleep "$SOAK_INTERVAL_SECONDS"
done

if awk -v rss="$max_rss" -v limit="$SOAK_MAX_RSS_MB" 'BEGIN {exit !(rss <= limit)}'; then
  mark "memory ceiling" "PASS" "max RSS ${max_rss}MB <= ${SOAK_MAX_RSS_MB}MB"
else
  mark "memory ceiling" "FAIL" "max RSS ${max_rss}MB > ${SOAK_MAX_RSS_MB}MB"
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
