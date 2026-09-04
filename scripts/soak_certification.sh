#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/soak-$(date -u +%Y%m%dT%H%M%SZ).md}"
TNG_HOST_URL="${TNG_HOST_URL:-http://localhost:${TNG_HOST_PORT:-18080}}"
TNG_API_TOKEN="${TNG_API_TOKEN:-local-cert-api-token-20260904}"
TNG_CONTAINER="${TNG_CONTAINER:-certification-torrentng-1}"
SOAK_DURATION_SECONDS="${SOAK_DURATION_SECONDS:-86400}"
SOAK_INTERVAL_SECONDS="${SOAK_INTERVAL_SECONDS:-60}"
SOAK_MAX_RSS_MB="${SOAK_MAX_RSS_MB:-500}"
SOAK_LIST_LIMIT="${SOAK_LIST_LIMIT:-50000}"
SOAK_MAX_FDS="${SOAK_MAX_FDS:-4096}"
SOAK_MAX_THREADS="${SOAK_MAX_THREADS:-512}"
SOAK_MIN_DISK_FREE_MB="${SOAK_MIN_DISK_FREE_MB:-100}"
SOAK_DATA_PATH="${SOAK_DATA_PATH:-/var/lib/torrentng}"
COOKIE_JAR="$(mktemp)"
BODY="$(mktemp)"
HEALTH_BODY="$(mktemp)"
METRICS_BODY="$(mktemp)"

mkdir -p "$(dirname "$OUT")"

mapped="$(docker port "$TNG_CONTAINER" 8080/tcp 2>/dev/null | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -1 || true)"
if [[ -n "$mapped" && "$TNG_HOST_URL" == http://localhost:* ]]; then
  TNG_HOST_URL="http://localhost:$mapped"
fi

cleanup() {
  rm -f "$COOKIE_JAR" "$BODY" "$HEALTH_BODY" "$METRICS_BODY"
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

process_field() {
  local field="$1"
  docker exec "$TNG_CONTAINER" sh -lc "awk -v field='$field' '\$1 == field {print \$2; exit}' /proc/1/status"
}

fd_count() {
  docker exec "$TNG_CONTAINER" sh -lc 'find /proc/1/fd -mindepth 1 -maxdepth 1 -type l 2>/dev/null | wc -l'
}

disk_free_mb() {
  docker exec "$TNG_CONTAINER" df -Pm "$SOAK_DATA_PATH" |
    awk 'NR == 2 {print $4; exit}'
}

{
  echo "# TorrentNG Soak Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- TorrentNG URL: $TNG_HOST_URL"
  echo "- Duration seconds: $SOAK_DURATION_SECONDS"
  echo "- Interval seconds: $SOAK_INTERVAL_SECONDS"
  echo "- Max RSS MB: $SOAK_MAX_RSS_MB"
  echo "- Max file descriptors: $SOAK_MAX_FDS"
  echo "- Max threads: $SOAK_MAX_THREADS"
  echo "- Minimum disk free MB: $SOAK_MIN_DISK_FREE_MB"
  echo "- Data path: $SOAK_DATA_PATH"
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
  echo "| UTC | Health | Torrents | RSS MB | sync/maindata HTTP | FDs | Threads | Disk free MB | Metrics HTTP | DB/Cache | Storage |"
  echo "|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---|"
} >> "$OUT"

deadline=$((SECONDS + SOAK_DURATION_SECONDS))
samples=0
max_rss="0"
bad_health=0
bad_sync=0
while (( SECONDS < deadline || samples == 0 )); do
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  health="$(curl -ksS -o "$HEALTH_BODY" -w '%{http_code}' \
    -H "Authorization: Bearer $TNG_API_TOKEN" "$TNG_HOST_URL/health" || true)"
  torrents="$(curl -ksS -b "$COOKIE_JAR" "$TNG_HOST_URL/api/qb/v2/torrents/info?limit=$SOAK_LIST_LIMIT" | jq 'length' 2>/dev/null || echo 0)"
  sync_code="$(curl -ksS -o "$BODY" -w '%{http_code}' -b "$COOKIE_JAR" "$TNG_HOST_URL/api/qb/v2/sync/maindata?rid=0" || true)"
  metrics_code="$(curl -ksS -o "$METRICS_BODY" -w '%{http_code}' \
    -H "Authorization: Bearer $TNG_API_TOKEN" "$TNG_HOST_URL/metrics" || true)"
  rss="$(rss_mb)"
  fds="$(fd_count)"
  threads="$(process_field VmThreads)"
  disk_free="$(disk_free_mb)"
  db_cache="$(jq -r '
    if .engine.subsystems.database_worker.healthy != null then
      (if .engine.subsystems.database_worker.healthy then "healthy" else "unhealthy" end)
    elif .cache == "ok" then "healthy"
    elif .cache != null then "unhealthy"
    else "n/a" end
  ' "$HEALTH_BODY" 2>/dev/null || echo unknown)"
  storage="$(jq -r '
    if .engine.subsystems.storage_workers.healthy != null then
      (if .engine.subsystems.storage_workers.healthy then "healthy" else "unhealthy" end)
    else "n/a" end
  ' "$HEALTH_BODY" 2>/dev/null || echo unknown)"
  if awk -v a="$rss" -v b="$max_rss" 'BEGIN {exit !(a > b)}'; then
    max_rss="$rss"
  fi
  printf '| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |\n' \
    "$now" "$health" "$torrents" "$rss" "$sync_code" "$fds" "$threads" \
    "$disk_free" "$metrics_code" "$db_cache" "$storage" >> "$OUT"
  [[ "$health" == "200" ]] || bad_health=$((bad_health + 1))
  [[ "$sync_code" == "200" ]] || bad_sync=$((bad_sync + 1))
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

max_fds="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $7); if ($7 + 0 > max) max = $7 + 0} END {print max + 0}' "$OUT")"
max_threads="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $8); if ($8 + 0 > max) max = $8 + 0} END {print max + 0}' "$OUT")"
min_disk="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $9); if (seen == 0 || ($9 + 0) < min) min = $9 + 0; seen = 1} END {print seen ? min : 0}' "$OUT")"
bad_metrics="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $10); if ($10 != "200") bad++} END {print bad + 0}' "$OUT")"
bad_components="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {for (i = 11; i <= 12; i++) {gsub(/[[:space:]]/, "", $i); if ($i == "unhealthy" || $i == "unknown") bad++}} END {print bad + 0}' "$OUT")"
if (( max_fds <= SOAK_MAX_FDS )); then
  mark "file-descriptor ceiling" "PASS" "max FDs ${max_fds} <= ${SOAK_MAX_FDS}"
else
  mark "file-descriptor ceiling" "FAIL" "max FDs ${max_fds} > ${SOAK_MAX_FDS}"
fi
if (( max_threads <= SOAK_MAX_THREADS )); then
  mark "thread ceiling" "PASS" "max threads ${max_threads} <= ${SOAK_MAX_THREADS}"
else
  mark "thread ceiling" "FAIL" "max threads ${max_threads} > ${SOAK_MAX_THREADS}"
fi
if (( min_disk >= SOAK_MIN_DISK_FREE_MB )); then
  mark "disk-free floor" "PASS" "min free ${min_disk}MB >= ${SOAK_MIN_DISK_FREE_MB}MB"
else
  mark "disk-free floor" "FAIL" "min free ${min_disk}MB < ${SOAK_MIN_DISK_FREE_MB}MB"
fi
if (( bad_metrics == 0 )); then
  mark "metrics endpoint" "PASS" "all samples returned HTTP 200"
else
  mark "metrics endpoint" "FAIL" "${bad_metrics} samples did not return HTTP 200"
fi
if (( bad_components == 0 )); then
  mark "dependency health fields" "PASS" "no unhealthy or unknown dependency samples"
else
  mark "dependency health fields" "FAIL" "${bad_components} unhealthy/unknown dependency fields"
fi
if (( bad_health == 0 )); then
  mark "health samples" "PASS" "all samples returned HTTP 200"
else
  mark "health samples" "FAIL" "${bad_health} samples did not return HTTP 200"
fi
if (( bad_sync == 0 )); then
  mark "sync samples" "PASS" "all samples returned HTTP 200"
else
  mark "sync samples" "FAIL" "${bad_sync} samples did not return HTTP 200"
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
