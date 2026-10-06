#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/curl_policy.sh
source "$ROOT/scripts/curl_policy.sh"
# shellcheck source=scripts/evidence_source.sh
source "$ROOT/scripts/evidence_source.sh"
OUT="${1:-$ROOT/certification/reports/soak-$(date -u +%Y%m%dT%H%M%SZ).md}"
SOURCE_COMMIT="$(evidence_source_commit "$ROOT")"
SOURCE_BRANCH="$(evidence_source_branch "$ROOT")"
SOURCE_WORKTREE_STATE="$(evidence_source_worktree_state "$ROOT")"
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
SOAK_EXPECTED_TORRENT_NAME="${SOAK_EXPECTED_TORRENT_NAME:-}"
SOAK_EXPECTED_TORRENT_HASH="${SOAK_EXPECTED_TORRENT_HASH:-}"
COOKIE_JAR="$(mktemp)"
BODY="$(mktemp)"
HEALTH_BODY="$(mktemp)"
METRICS_BODY="$(mktemp)"
TORRENTS_BODY="$(mktemp)"
AUTH_HEADER_FILE="$(mktemp)"

cleanup() {
  rm -f "$COOKIE_JAR" "$BODY" "$HEALTH_BODY" "$METRICS_BODY" "$TORRENTS_BODY" "$AUTH_HEADER_FILE"
}
trap cleanup EXIT

if [[ ! "$TNG_CONTAINER" =~ ^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$ ]]; then
  echo "TNG_CONTAINER must be a Docker container name or ID" >&2
  exit 2
fi
if [[ "$SOAK_DATA_PATH" != /* || "$SOAK_DATA_PATH" == *$'\n'* || "$SOAK_DATA_PATH" == *$'\r'* ]]; then
  echo "SOAK_DATA_PATH must be an absolute path without line breaks" >&2
  exit 2
fi

mapped="$(docker port "$TNG_CONTAINER" 8080/tcp 2>/dev/null | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -1 || true)"
if [[ -n "$mapped" && "$TNG_HOST_URL" == http://localhost:* ]]; then
  TNG_HOST_URL="http://localhost:$mapped"
fi
CONTAINER_ID="$(docker inspect --format '{{.Id}}' "$TNG_CONTAINER" 2>/dev/null || true)"
CONTAINER_IMAGE_ID="$(docker inspect --format '{{.Image}}' "$TNG_CONTAINER" 2>/dev/null || true)"
CONTAINER_SOURCE_REVISION="$(docker inspect --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}' "$TNG_CONTAINER" 2>/dev/null || true)"

python3 "$ROOT/scripts/protected_target.py" "$TNG_HOST_URL"
if [[ "$TNG_API_TOKEN" == *$'\n'* || "$TNG_API_TOKEN" == *$'\r'* ]]; then
  echo "TNG_API_TOKEN must not contain line breaks" >&2
  exit 2
fi
printf 'Authorization: Bearer %s\n' "$TNG_API_TOKEN" > "$AUTH_HEADER_FILE"
chmod 0600 "$AUTH_HEADER_FILE"

curl_protected() {
  curl -q --silent --show-error --noproxy '*' \
    --connect-timeout 5 --max-time 20 "$@"
}

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

disk_free_mb() {
  docker exec "$TNG_CONTAINER" df -Pm -- "$SOAK_DATA_PATH" |
    awk 'NR == 2 {print $4; exit}'
}

{
  echo "# TorrentNG Soak Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Source commit: $SOURCE_COMMIT"
  echo "- Source branch: $SOURCE_BRANCH"
  echo "- Source worktree state: $SOURCE_WORKTREE_STATE"
  echo "- TorrentNG URL: $TNG_HOST_URL"
  echo "- Container ID: ${CONTAINER_ID:-unavailable}"
  echo "- Container image ID: ${CONTAINER_IMAGE_ID:-unavailable}"
  echo "- Container source revision: ${CONTAINER_SOURCE_REVISION:-unavailable}"
  echo "- Duration seconds: $SOAK_DURATION_SECONDS"
  echo "- Interval seconds: $SOAK_INTERVAL_SECONDS"
  echo "- Max RSS MB: $SOAK_MAX_RSS_MB"
  echo "- Max file descriptors: $SOAK_MAX_FDS"
  echo "- Max threads: $SOAK_MAX_THREADS"
  echo "- Minimum disk free MB: $SOAK_MIN_DISK_FREE_MB"
  echo "- Data path: $SOAK_DATA_PATH"
  echo "- List limit: $SOAK_LIST_LIMIT"
  echo "- Expected torrent name: ${SOAK_EXPECTED_TORRENT_NAME:-none}"
  echo "- Expected torrent hash: ${SOAK_EXPECTED_TORRENT_HASH:-none}"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

if [[ "$SOURCE_WORKTREE_STATE" == "clean" && "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ &&
  "$CONTAINER_SOURCE_REVISION" == "$SOURCE_COMMIT" ]]; then
  mark "source provenance" "PASS" "clean source commit matches the container image revision"
else
  mark "source provenance" "FAIL" "clean source commit and matching image revision are required; no soak samples were collected"
  echo >> "$OUT"
  echo "Overall status: $status" >> "$OUT"
  echo "$OUT"
  exit 1
fi

auth_payload="$(python3 -c 'import os; from urllib.parse import urlencode; token = os.environ["TNG_API_TOKEN"]; print(urlencode({"username": token, "password": token}))')"
code="$(printf '%s' "$auth_payload" | curl_protected -o "$BODY" -w '%{http_code}' \
  "$TNG_HOST_URL/api/qb/v2/auth/login" -X POST \
  -H 'Content-Type: application/x-www-form-urlencoded' \
  --data-binary @- -c "$COOKIE_JAR" || true)"
unset auth_payload
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
  echo "| UTC | Health | Torrents | RSS MB | sync/maindata HTTP | FDs | Threads | Disk free MB | Metrics HTTP | DB/Cache | Storage | Daemon PID | Executable |"
  echo "|---|---:|---:|---:|---:|---:|---:|---:|---|---|---:|---|---|"
} >> "$OUT"

deadline=$((SECONDS + SOAK_DURATION_SECONDS))
samples=0
max_rss="0"
bad_health=0
bad_sync=0
bad_expected=0
bad_sampler=0
while (( SECONDS < deadline || samples == 0 )); do
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  health="$(curl_protected -o "$HEALTH_BODY" -w '%{http_code}' \
    -H "@$AUTH_HEADER_FILE" "$TNG_HOST_URL/health" || true)"
  curl_protected -o "$TORRENTS_BODY" -b "$COOKIE_JAR" \
    "$TNG_HOST_URL/api/qb/v2/torrents/info?limit=$SOAK_LIST_LIMIT" || true
  torrents="$(jq 'length' "$TORRENTS_BODY" 2>/dev/null || echo 0)"
  if [[ -n "$SOAK_EXPECTED_TORRENT_NAME" ]]; then
    if jq -e --arg name "$SOAK_EXPECTED_TORRENT_NAME" --arg hash "$SOAK_EXPECTED_TORRENT_HASH" '
      type == "array" and any(.[];
        (.name // "") == $name
        and ($hash == "" or (((.hash // .infohash_v1 // "") | ascii_downcase) == ($hash | ascii_downcase)))
        and ((.progress // 0) >= 0.999)
        and ((.state // "") | IN("uploading", "seeding", "stalledUP"))
      )
    ' "$TORRENTS_BODY" >/dev/null 2>&1; then
      :
    else
      bad_expected=$((bad_expected + 1))
    fi
  fi
  sync_code="$(curl_protected -o "$BODY" -w '%{http_code}' -b "$COOKIE_JAR" "$TNG_HOST_URL/api/qb/v2/sync/maindata?rid=0" || true)"
  metrics_code="$(curl_protected -o "$METRICS_BODY" -w '%{http_code}' \
    -H "@$AUTH_HEADER_FILE" "$TNG_HOST_URL/metrics" || true)"
  daemon_sample=""
  daemon_pid="unavailable"
  daemon_exe="unavailable"
  rss="0"
  fds="0"
  threads="0"
  daemon_candidates="$(docker top "$TNG_CONTAINER" -eo pid,comm,args 2>/dev/null | awk 'NR > 1 && $2 == "torrentngd" && $3 ~ /(^|\/)torrentngd$/ {print $1 "\t" $3}')"
  daemon_candidate_count="$(printf '%s\n' "$daemon_candidates" | awk 'NF {count++} END {print count + 0}')"
  daemon_host_pid=""
  daemon_host_exe=""
  if [[ "$daemon_candidate_count" == 1 ]]; then
    IFS=$'\t' read -r daemon_host_pid daemon_host_exe <<< "$daemon_candidates"
  fi
  if [[ "$daemon_candidate_count" == 1 && "$daemon_host_pid" =~ ^[0-9]+$ ]] &&
    daemon_sample="$(SOAK_PROC_ROOT=/proc "$ROOT/scripts/soak_process_sample.sh" --host-process "$daemon_host_pid" "$daemon_host_exe")"; then
    IFS=$'\t' read -r daemon_pid daemon_exe rss_kib fds threads <<< "$daemon_sample"
    if [[ "$daemon_pid" =~ ^[0-9]+$ && "$daemon_exe" == */torrentngd && "$rss_kib" =~ ^[0-9]+$ && "$fds" =~ ^[0-9]+$ && "$threads" =~ ^[0-9]+$ ]]; then
      rss="$(awk -v kib="$rss_kib" 'BEGIN {printf "%.1f", kib / 1024}')"
    else
      bad_sampler=$((bad_sampler + 1))
      daemon_pid="invalid"
      daemon_exe="invalid sample"
    fi
  else
    bad_sampler=$((bad_sampler + 1))
  fi
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
  printf '| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |\n' \
    "$now" "$health" "$torrents" "$rss" "$sync_code" "$fds" "$threads" \
    "$disk_free" "$metrics_code" "$db_cache" "$storage" "$daemon_pid" "$daemon_exe" >> "$OUT"
  [[ "$health" == "200" ]] || bad_health=$((bad_health + 1))
  [[ "$sync_code" == "200" ]] || bad_sync=$((bad_sync + 1))
  samples=$((samples + 1))
  if (( SECONDS >= deadline )); then
    break
  fi
  sleep "$SOAK_INTERVAL_SECONDS"
done

if [[ -n "$CONTAINER_ID" && -n "$CONTAINER_IMAGE_ID" ]]; then
  mark "artifact identity" "PASS" "container ID, image ID, and source revision recorded"
else
  mark "artifact identity" "FAIL" "container or image ID unavailable; resource samples are not bound to an artifact"
fi

if (( bad_sampler > 0 )); then
  mark "daemon resource sampler" "FAIL" "$bad_sampler samples did not resolve exactly one torrentngd process; daemon resource ceilings are invalid"
elif awk -v rss="$max_rss" -v limit="$SOAK_MAX_RSS_MB" 'BEGIN {exit !(rss <= limit)}'; then
  mark "daemon resource sampler" "PASS" "all $samples samples identified torrentngd by executable; PID and artifact identity recorded"
  mark "memory ceiling" "PASS" "max RSS ${max_rss}MB <= ${SOAK_MAX_RSS_MB}MB"
else
  mark "memory ceiling" "FAIL" "max RSS ${max_rss}MB > ${SOAK_MAX_RSS_MB}MB"
fi

max_fds="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $7); if ($7 + 0 > max) max = $7 + 0} END {print max + 0}' "$OUT")"
max_threads="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $8); if ($8 + 0 > max) max = $8 + 0} END {print max + 0}' "$OUT")"
min_disk="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $9); if (seen == 0 || ($9 + 0) < min) min = $9 + 0; seen = 1} END {print seen ? min : 0}' "$OUT")"
bad_metrics="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {gsub(/[[:space:]]/, "", $10); if ($10 != "200") bad++} END {print bad + 0}' "$OUT")"
bad_components="$(awk -F'|' '/^\| 20[0-9][0-9]-/ {for (i = 11; i <= 12; i++) {gsub(/[[:space:]]/, "", $i); if ($i == "unhealthy" || $i == "unknown") bad++}} END {print bad + 0}' "$OUT")"
if (( bad_sampler > 0 )); then
  mark "file-descriptor ceiling" "FAIL" "daemon FD sampling incomplete; no ceiling can be claimed"
elif (( max_fds <= SOAK_MAX_FDS )); then
  mark "file-descriptor ceiling" "PASS" "max FDs ${max_fds} <= ${SOAK_MAX_FDS}"
else
  mark "file-descriptor ceiling" "FAIL" "max FDs ${max_fds} > ${SOAK_MAX_FDS}"
fi
if (( bad_sampler > 0 )); then
  mark "thread ceiling" "FAIL" "daemon thread sampling incomplete; no ceiling can be claimed"
elif (( max_threads <= SOAK_MAX_THREADS )); then
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
if [[ -n "$SOAK_EXPECTED_TORRENT_NAME" ]]; then
  if (( bad_expected == 0 )); then
    mark "expected public torrent" "PASS" "${samples} samples retained the expected completed torrent"
  else
    mark "expected public torrent" "FAIL" "${bad_expected} samples lost or regressed the expected torrent"
  fi
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
