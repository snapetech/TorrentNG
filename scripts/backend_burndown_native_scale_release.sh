#!/usr/bin/env bash
set -euo pipefail

# Run a 100k-row restore/list/stats/promotion/demotion check against the
# optimized production daemon. The dataset is deliberately synthetic, but it
# goes through the real SQLite restore path and the real torrentngd binary.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/backend-burndown-native-scale-release-$(date -u +%Y%m%dT%H%M%SZ).md}"
PREFIX="${OUT%.md}"
ARTIFACT_DIR="${PREFIX}.artifacts"
BIN="${TNG_RELEASE_BINARY:-$ROOT/target/release/torrentngd}"
STATIC_DIR="${TNG_STATIC_DIR:-$ROOT/sidecar/static}"
API_PORT="${TNG_SCALE_API_PORT:-28082}"
PEER_PORT="${TNG_SCALE_PEER_PORT:-45557}"
TOKEN="${TNG_SCALE_TOKEN:-backend-burndown-native-scale-token-20260902}"
TORRENT_COUNT="${TNG_SCALE_TORRENTS:-100000}"

mkdir -p "$(dirname "$OUT")"
test -x "$BIN"
for command in awk curl find jq sha1sum sha256sum sqlite3 stat truncate xxd; do
  command -v "$command" >/dev/null
done
[[ "$TORRENT_COUNT" =~ ^[0-9]+$ ]] && (( TORRENT_COUNT >= 2 ))

port_in_use() {
  ss -ltnH 2>/dev/null | awk -v port=":$1" '$4 ~ port "$" { found = 1 } END { exit !found }'
}

if port_in_use "$API_PORT"; then
  echo "API port $API_PORT is already in use; set TNG_SCALE_API_PORT" >&2
  exit 1
fi
if port_in_use "$PEER_PORT"; then
  echo "peer port $PEER_PORT is already in use; set TNG_SCALE_PEER_PORT" >&2
  exit 1
fi

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/torrentng-native-scale.XXXXXX")"
SESSION_DIR="$TMP_DIR/session"
DOWNLOAD_DIR="$TMP_DIR/downloads"
DB="$SESSION_DIR/state.db"
CONFIG="$TMP_DIR/config.toml"
PAYLOAD_NAME="torrentng-scale-payload"
PAYLOAD="$DOWNLOAD_DIR/$PAYLOAD_NAME"
INFO_BENCODE="$TMP_DIR/info.bencode"
TORRENT_BLOB="$TMP_DIR/promotion.torrent"
LOG_BOOTSTRAP="$TMP_DIR/bootstrap.log"
LOG_POPULATED="$TMP_DIR/populated.log"
LOG_RESTART="$TMP_DIR/restart.log"

mkdir -p "$SESSION_DIR/torrents" "$DOWNLOAD_DIR"

daemon_pid=""
last_shutdown_polls=0

cleanup() {
  if [[ -n "$daemon_pid" ]]; then
    stop_daemon || true
  fi
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

write_config() {
  cat >"$CONFIG" <<EOF
[daemon]
session_dir = "$SESSION_DIR"
api_bind = "127.0.0.1:$API_PORT"
log_level = "warn"
shutdown_timeout_secs = 10

[network]
listen_port = $PEER_PORT
max_peers = 200

[storage]
download_dir = "$DOWNLOAD_DIR"

[runtime]
torrent_tiers_enabled = true
tier_hot_idle_secs = 1
tier_warm_idle_secs = 3

[dht]
enabled = false

[db]
path = "$DB"

[auth]
api_tokens = ["$TOKEN"]
EOF
}

start_daemon() {
  local log_path="$1"
  TORRENTNGD_CONFIG="$CONFIG" TNG_STATIC_DIR="$STATIC_DIR" "$BIN" >"$log_path" 2>&1 &
  daemon_pid=$!
}

stop_daemon() {
  local polls=0
  if [[ -n "$daemon_pid" ]] && kill -0 "$daemon_pid" 2>/dev/null; then
    kill -TERM "$daemon_pid" 2>/dev/null || true
    for _ in $(seq 1 100); do
      polls=$((polls + 1))
      if ! kill -0 "$daemon_pid" 2>/dev/null; then
        break
      fi
      sleep 0.1
    done
  fi
  if [[ -n "$daemon_pid" ]]; then
    wait "$daemon_pid" 2>/dev/null || true
  fi
  last_shutdown_polls="$polls"
  daemon_pid=""
}

wait_ready() {
  local health_url="http://127.0.0.1:$API_PORT/health"
  for _ in $(seq 1 200); do
    if curl -fsS --connect-timeout 1 --max-time 3 \
      -H "Authorization: Bearer $TOKEN" "$health_url" -o /dev/null 2>/dev/null; then
      return 0
    fi
    if [[ -n "$daemon_pid" ]] && ! kill -0 "$daemon_pid" 2>/dev/null; then
      return 1
    fi
    sleep 0.1
  done
  return 1
}

measure_process() {
  local pid="$1"
  PROCESS_RSS_BYTES="$(awk '/^VmRSS:/ { print $2 * 1024; exit }' "/proc/$pid/status")"
  PROCESS_THREADS="$(awk '/^Threads:/ { print $2; exit }' "/proc/$pid/status")"
  PROCESS_FDS="$(find "/proc/$pid/fd" -mindepth 1 -maxdepth 1 -type l 2>/dev/null | wc -l | tr -d ' ' )"
}

curl_get_timed() {
  local url="$1"
  local output="$2"
  CURL_SECONDS="$(curl -fsS --connect-timeout 5 --max-time 60 \
    -H "Authorization: Bearer $TOKEN" -o "$output" -w '%{time_total}' "$url")"
  CURL_MILLISECONDS="$(awk -v seconds="$CURL_SECONDS" 'BEGIN { printf "%.3f", seconds * 1000 }')"
}

curl_post_timed() {
  local url="$1"
  local output="$2"
  CURL_SECONDS="$(curl -fsS --connect-timeout 5 --max-time 60 \
    -X POST -H "Authorization: Bearer $TOKEN" -o "$output" -w '%{time_total}' "$url")"
  CURL_MILLISECONDS="$(awk -v seconds="$CURL_SECONDS" 'BEGIN { printf "%.3f", seconds * 1000 }')"
}

metric_value() {
  awk -v metric="$2" '$1 == metric { print $2; exit }' "$1"
}

write_config

# Build a valid single-file v1 torrent so promotion runs the real metadata,
# recheck, actor, and tier paths. The payload is zero-filled and its SHA-1 is
# embedded as the only piece hash.
truncate -s 16 "$PAYLOAD"
piece_hash="$(sha1sum "$PAYLOAD" | awk '{ print $1 }')"
{
  printf 'd6:lengthi16e4:name%d:%s12:piece lengthi16e6:pieces20:' "${#PAYLOAD_NAME}" "$PAYLOAD_NAME"
  printf '%s' "$piece_hash" | xxd -r -p
  printf '7:privatei1ee'
} >"$INFO_BENCODE"
PROMOTE_HASH="$(sha1sum "$INFO_BENCODE" | awk '{ print $1 }')"
{
  printf 'd4:info'
  cat "$INFO_BENCODE"
  printf 'e'
} >"$TORRENT_BLOB"
cp "$TORRENT_BLOB" "$SESSION_DIR/torrents/$PROMOTE_HASH.torrent"

# Bootstrap through the daemon itself so the database is created by the real
# migration path. It is stopped before seeding, so no external writer races
# the measurement run.
start_daemon "$LOG_BOOTSTRAP"
if ! wait_ready; then
  echo "bootstrap daemon did not become ready; see $LOG_BOOTSTRAP" >&2
  exit 1
fi
stop_daemon
test -f "$DB"

cold_count=$((TORRENT_COUNT - 1))
seed_started_ms="$(date +%s%3N)"
sqlite3 "$DB" >/dev/null <<SQL
PRAGMA busy_timeout = 5000;
PRAGMA journal_mode = WAL;
BEGIN IMMEDIATE;
WITH RECURSIVE sequence(n) AS (
    SELECT 1
    UNION ALL
    SELECT n + 1 FROM sequence WHERE n < $cold_count
)
INSERT INTO torrents
    (info_hash, name, total_length, piece_length, piece_count, is_private,
     save_path, category, tags, state, added_at, completed_at,
     uploaded, downloaded, ratio, trackers)
SELECT
    printf('00000000000000000000000000000000%08x', n),
    printf('scale-cold-%06d', n),
    16, 16, 1, 1,
    '$DOWNLOAD_DIR', NULL, '[]', 'stopped',
    strftime('%s', 'now') - n, NULL, 0, 0, 0.0, '[]'
FROM sequence;
INSERT INTO torrents
    (info_hash, name, total_length, piece_length, piece_count, is_private,
     save_path, category, tags, state, added_at, completed_at,
     uploaded, downloaded, ratio, trackers)
VALUES
    ('$PROMOTE_HASH', '$PAYLOAD_NAME', 16, 16, 1, 1,
     '$DOWNLOAD_DIR', NULL, '[]', 'seeding', strftime('%s', 'now'),
     strftime('%s', 'now'), 0, 16, 0.0, '[]');
COMMIT;
SQL
seed_duration_ms="$(( $(date +%s%3N) - seed_started_ms ))"
seeded_count="$(sqlite3 "$DB" 'SELECT COUNT(*) FROM torrents;')"
[[ "$seeded_count" == "$TORRENT_COUNT" ]]

BASE_URL="http://127.0.0.1:$API_PORT"
start_started_ms="$(date +%s%3N)"
start_daemon "$LOG_POPULATED"
if ! wait_ready; then
  echo "populated daemon did not become ready; see $LOG_POPULATED" >&2
  exit 1
fi
ready_duration_ms="$(( $(date +%s%3N) - start_started_ms ))"
measure_process "$daemon_pid"
restore_rss_bytes="$PROCESS_RSS_BYTES"
restore_threads="$PROCESS_THREADS"
restore_fds="$PROCESS_FDS"

curl_get_timed "$BASE_URL/health" "$TMP_DIR/health.json"
health_latency_ms="$CURL_MILLISECONDS"
jq -e --argjson expected "$TORRENT_COUNT" \
  '.ready == true and .torrent_count == $expected and .engine.subsystems.engine.alive == true and .engine.subsystems.storage_workers.healthy == true' \
  "$TMP_DIR/health.json" >/dev/null

curl_get_timed "$BASE_URL/api/v1/torrents?limit=2" "$TMP_DIR/native-page-1.json"
native_page_1_latency_ms="$CURL_MILLISECONDS"
jq -e --argjson expected "$TORRENT_COUNT" \
  '.total == $expected and (.torrents | length) == 2' "$TMP_DIR/native-page-1.json" >/dev/null
snapshot="$(jq -r '.snapshot' "$TMP_DIR/native-page-1.json")"
curl_get_timed "$BASE_URL/api/v1/torrents?limit=2&offset=2&snapshot=$snapshot" "$TMP_DIR/native-page-2.json"
native_page_2_latency_ms="$CURL_MILLISECONDS"
jq -e --argjson expected "$TORRENT_COUNT" --argjson expected_snapshot "$snapshot" \
  '.total == $expected and (.torrents | length) == 2 and .snapshot == $expected_snapshot' \
  "$TMP_DIR/native-page-2.json" >/dev/null

curl_get_timed "$BASE_URL/api/qb/v2/torrents/info?limit=200" "$TMP_DIR/qbit-page.json"
qbit_page_latency_ms="$CURL_MILLISECONDS"
qbit_limit=$((TORRENT_COUNT < 200 ? TORRENT_COUNT : 200))
jq -e --argjson expected "$qbit_limit" 'type == "array" and length == $expected' "$TMP_DIR/qbit-page.json" >/dev/null

curl_get_timed "$BASE_URL/api/v1/transfer/info" "$TMP_DIR/native-transfer.json"
native_transfer_latency_ms="$CURL_MILLISECONDS"
jq -e 'has("dl_info_speed") and has("up_info_speed")' "$TMP_DIR/native-transfer.json" >/dev/null

curl_get_timed "$BASE_URL/api/qb/v2/transfer/info" "$TMP_DIR/qbit-transfer.json"
qbit_transfer_latency_ms="$CURL_MILLISECONDS"
jq -e 'has("dl_info_speed") and has("up_info_speed")' "$TMP_DIR/qbit-transfer.json" >/dev/null

curl_get_timed "$BASE_URL/metrics" "$TMP_DIR/metrics-restore.txt"
metrics_restore_latency_ms="$CURL_MILLISECONDS"
grep -Eq "^torrentng_torrents_total $TORRENT_COUNT(\\.0)?$" "$TMP_DIR/metrics-restore.txt"
grep -q '^torrentng_storage_workers_healthy 1$' "$TMP_DIR/metrics-restore.txt"
restore_active_tasks="$(metric_value "$TMP_DIR/metrics-restore.txt" torrentng_torrent_tasks_active)"
[[ "$restore_active_tasks" == "0" ]]

pre_promotion_threads="$restore_threads"
promotion_started_ms="$(date +%s%3N)"
curl_post_timed "$BASE_URL/api/v1/torrents/$PROMOTE_HASH/resume" "$TMP_DIR/promotion-response.json"
promotion_request_latency_ms="$CURL_MILLISECONDS"
promotion_state=""
promotion_polls=0
for _ in $(seq 1 120); do
  promotion_polls=$((promotion_polls + 1))
  if curl -fsS --connect-timeout 2 --max-time 5 \
    -H "Authorization: Bearer $TOKEN" \
    "$BASE_URL/api/v1/torrents/$PROMOTE_HASH" -o "$TMP_DIR/promoted-detail.json" 2>/dev/null; then
    promotion_state="$(jq -r '.state // empty' "$TMP_DIR/promoted-detail.json")"
    if [[ "$promotion_state" == "seeding" ]]; then
      break
    fi
  fi
  sleep 0.1
done
promotion_duration_ms="$(( $(date +%s%3N) - promotion_started_ms ))"
[[ "$promotion_state" == "seeding" ]]
measure_process "$daemon_pid"
promoted_threads="$PROCESS_THREADS"
promoted_rss_bytes="$PROCESS_RSS_BYTES"
promotion_metric_started_ms="$(date +%s%3N)"
promotion_metric_polls=0
promotion_hot="0"
while (( $(date +%s%3N) - promotion_metric_started_ms < 5000 )); do
  promotion_metric_polls=$((promotion_metric_polls + 1))
  curl_get_timed "$BASE_URL/metrics" "$TMP_DIR/metrics-promoted.txt"
  metrics_promoted_latency_ms="$CURL_MILLISECONDS"
  promotion_hot="$(metric_value "$TMP_DIR/metrics-promoted.txt" torrentng_torrents_activity_hot)"
  if [[ "$promotion_hot" == "1" ]]; then
    break
  fi
  sleep 0.1
done
promotion_metric_wait_ms="$(( $(date +%s%3N) - promotion_metric_started_ms ))"
[[ "$promotion_hot" == "1" ]]
promotion_active_tasks="$(metric_value "$TMP_DIR/metrics-promoted.txt" torrentng_torrent_tasks_active)"
[[ "$promotion_active_tasks" == "1" ]]

# The test config shortens the production tier policy only for this evidence
# run. The normal defaults remain 120s Hot and 1800s Warm. Reconcile runs on
# the engine timer, so allow enough wall time for one full pass.
demotion_started_ms="$(date +%s%3N)"
demotion_polls=0
demotion_hot="1"
demotion_dormant=""
while (( $(date +%s%3N) - demotion_started_ms < 15000 )); do
  demotion_polls=$((demotion_polls + 1))
  curl -fsS --connect-timeout 2 --max-time 5 \
    -H "Authorization: Bearer $TOKEN" "$BASE_URL/metrics" -o "$TMP_DIR/metrics-demotion.txt"
  demotion_hot="$(metric_value "$TMP_DIR/metrics-demotion.txt" torrentng_torrents_activity_hot)"
  demotion_dormant="$(metric_value "$TMP_DIR/metrics-demotion.txt" torrentng_torrents_activity_dormant)"
  demotion_active_tasks="$(metric_value "$TMP_DIR/metrics-demotion.txt" torrentng_torrent_tasks_active)"
  if [[ "$demotion_hot" == "0" && "$demotion_dormant" == "$TORRENT_COUNT" && "$demotion_active_tasks" == "0" ]]; then
    break
  fi
  sleep 0.25
done
demotion_duration_ms="$(( $(date +%s%3N) - demotion_started_ms ))"
[[ "$demotion_hot" == "0" && "$demotion_dormant" == "$TORRENT_COUNT" && "$demotion_active_tasks" == "0" ]]
measure_process "$daemon_pid"
demoted_threads="$PROCESS_THREADS"
demoted_rss_bytes="$PROCESS_RSS_BYTES"

stop_daemon
populated_shutdown_polls="$last_shutdown_polls"

restart_started_ms="$(date +%s%3N)"
start_daemon "$LOG_RESTART"
if ! wait_ready; then
  echo "restart daemon did not become ready; see $LOG_RESTART" >&2
  exit 1
fi
restart_ready_duration_ms="$(( $(date +%s%3N) - restart_started_ms ))"
measure_process "$daemon_pid"
restart_rss_bytes="$PROCESS_RSS_BYTES"
restart_threads="$PROCESS_THREADS"
restart_fds="$PROCESS_FDS"
curl_get_timed "$BASE_URL/health" "$TMP_DIR/restart-health.json"
restart_health_latency_ms="$CURL_MILLISECONDS"
jq -e --argjson expected "$TORRENT_COUNT" \
  '.ready == true and .torrent_count == $expected and .engine.subsystems.storage_workers.healthy == true' \
  "$TMP_DIR/restart-health.json" >/dev/null
curl_get_timed "$BASE_URL/metrics" "$TMP_DIR/metrics-restart.txt"
metrics_restart_latency_ms="$CURL_MILLISECONDS"
restart_hot="$(metric_value "$TMP_DIR/metrics-restart.txt" torrentng_torrents_activity_hot)"
restart_dormant="$(metric_value "$TMP_DIR/metrics-restart.txt" torrentng_torrents_activity_dormant)"
restart_active_tasks="$(metric_value "$TMP_DIR/metrics-restart.txt" torrentng_torrent_tasks_active)"
[[ "$restart_hot" == "0" && "$restart_dormant" == "$TORRENT_COUNT" && "$restart_active_tasks" == "0" ]]
stop_daemon
restart_shutdown_polls="$last_shutdown_polls"

binary_bytes="$(stat -c '%s' "$BIN")"
binary_sha256="$(sha256sum "$BIN" | awk '{ print $1 }')"
metrics_lines="$(wc -l <"$TMP_DIR/metrics-restore.txt" | tr -d ' ')"
metrics_bytes="$(wc -c <"$TMP_DIR/metrics-restore.txt" | tr -d ' ')"
if git -C "$ROOT" diff --quiet --ignore-submodules -- && git -C "$ROOT" diff --cached --quiet --ignore-submodules --; then
  worktree="clean"
else
  worktree="dirty"
fi

mkdir -p "$ARTIFACT_DIR"
cp "$CONFIG" "$ARTIFACT_DIR/config.toml"
cp "$LOG_BOOTSTRAP" "$ARTIFACT_DIR/bootstrap.log"
cp "$LOG_POPULATED" "$ARTIFACT_DIR/populated.log"
cp "$LOG_RESTART" "$ARTIFACT_DIR/restart.log"
cp "$TMP_DIR"/*.json "$TMP_DIR"/*.txt "$ARTIFACT_DIR/"

{
  echo "# TorrentNG Native Release-Binary 100k Scale Evidence"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Kernel: $(uname -srmo)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo "- Binary: $BIN"
  echo "- Binary bytes: $binary_bytes"
  echo "- Binary SHA-256: $binary_sha256"
  echo "- Worktree: $worktree"
  echo "- Dataset: $TORRENT_COUNT file-backed SQLite rows; $cold_count dormant stopped rows plus one valid seeding row"
  echo "- Synthetic promotion hash: $PROMOTE_HASH"
  echo "- Raw artifacts: $(basename "$ARTIFACT_DIR")/"
  echo
  echo "This is a production-daemon measurement using a synthetic SQLite corpus.
It proves restore and API behavior for this host and build; it does not prove
capacity under real torrent metadata diversity, peer traffic, tracker load,
filesystem contention, or a production soak."
  echo
  echo "## Restore and process footprint"
  echo
  echo "| Measurement | Result |"
  echo "|---|---:|"
  echo "| SQLite seed duration | ${seed_duration_ms} ms |"
  echo "| Populated startup to health | ${ready_duration_ms} ms |"
  echo "| Health request | ${health_latency_ms} ms |"
  echo "| RSS after restore | ${restore_rss_bytes} bytes |"
  echo "| File descriptors after restore | $restore_fds |"
  echo "| Linux threads after restore | $restore_threads |"
  echo "| Metrics lines / bytes | $metrics_lines / $metrics_bytes |"
  echo
  echo "## Bounded API and aggregate stats"
  echo
  echo "| Request | Result |"
  echo "|---|---:|"
  echo "| Native page 1 (limit=2, total=$TORRENT_COUNT) | ${native_page_1_latency_ms} ms |"
  echo "| Native page 2 (same snapshot) | ${native_page_2_latency_ms} ms |"
  echo "| qBittorrent page (limit=200) | ${qbit_page_latency_ms} ms |"
  echo "| Native transfer info | ${native_transfer_latency_ms} ms |"
  echo "| qBittorrent transfer info | ${qbit_transfer_latency_ms} ms |"
  echo "| Restore metrics | ${metrics_restore_latency_ms} ms |"
  echo
  echo "## Runtime tier exercise"
  echo
  echo "| Transition | Result |"
  echo "|---|---:|"
  echo "| Pre-promotion Linux threads | $pre_promotion_threads |"
  echo "| Resume request | ${promotion_request_latency_ms} ms |"
  echo "| Dormant to Hot/Seeding promotion | PASS; ${promotion_duration_ms} ms; $promotion_polls polls |"
  echo "| Linux threads after promotion | $promoted_threads |"
  echo "| RSS after promotion | ${promoted_rss_bytes} bytes |"
  echo "| Hot tier after promotion | $promotion_hot |"
  echo "| Active torrent-task gauge after promotion | $promotion_active_tasks |"
  echo "| Stats refresh wait for Hot metric | ${promotion_metric_wait_ms} ms; $promotion_metric_polls polls |"
  echo "| Hot to Dormant demotion | PASS; ${demotion_duration_ms} ms; $demotion_polls polls; active_tasks=$demotion_active_tasks |"
  echo "| Linux threads after demotion | $demoted_threads |"
  echo "| RSS after demotion | ${demoted_rss_bytes} bytes |"
  echo
  echo "## Restart"
  echo
  echo "| Measurement | Result |"
  echo "|---|---:|"
  echo "| Restart to health | ${restart_ready_duration_ms} ms |"
  echo "| Restart health request | ${restart_health_latency_ms} ms |"
  echo "| Restart RSS | ${restart_rss_bytes} bytes |"
  echo "| Restart file descriptors | $restart_fds |"
  echo "| Restart Linux threads | $restart_threads |"
  echo "| Restart active torrent-task gauge | $restart_active_tasks |"
  echo "| Restart tier state | PASS; hot=$restart_hot dormant=$restart_dormant |"
  echo "| Populated shutdown | PASS; $populated_shutdown_polls polls |"
  echo "| Restart shutdown | PASS; $restart_shutdown_polls polls |"
  echo
  echo "## Remaining evidence gap"
  echo
  echo "- The corpus is synthetic and all cold rows share one save root, one piece shape, no trackers, and no peer traffic."
  echo "- This run does not establish 100k production readiness for real metadata diversity, tracker deadlines, SSE fan-out, slow clients, storage failures, crash recovery, or 24-hour stability."
  echo "- TNG-029 persistence isolation and local live fault injection are complete in the current tree; this script measures the separate TNG-010 capacity proxy only and does not create a 100k production-readiness claim."
  echo
  echo "Overall status: PASS_WITH_LIMITATIONS"
} >"$OUT"

echo "$OUT"
