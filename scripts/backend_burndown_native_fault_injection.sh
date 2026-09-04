#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/backend-burndown-native-fault-$(date -u +%Y%m%dT%H%M%SZ).md}"
BIN="${TNG_FAULT_BINARY:-$ROOT/target/release/torrentngd}"
STATIC_DIR="${TNG_STATIC_DIR:-$ROOT/sidecar/static}"
BASE_CONFIG="${TNG_FAULT_CONFIG:-$ROOT/certification/fixtures/backend-burndown-native-release-smoke.toml}"
TOKEN="${TNG_FAULT_TOKEN:-backend-burndown-token-20260902}"
STORAGE_DELAY_MS="${TNG_FAULT_STORAGE_DELAY_MS:-2000}"

mkdir -p "$(dirname "$OUT")"
test -x "$BIN"
test -f "$BASE_CONFIG"
command -v curl >/dev/null
command -v python3 >/dev/null

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/torrentng-fault.XXXXXX")"
EVIDENCE_DIR="${OUT%.md}.artifacts"
daemon_pid=""
config="$BASE_CONFIG"
url="${TNG_FAULT_URL:-http://127.0.0.1:28080}"
db_path="${TNG_FAULT_DB_PATH:-}"
first_log="$WORK_DIR/first.log"
second_log="$WORK_DIR/second.log"
health_one="$WORK_DIR/health-one.json"
health_two="$WORK_DIR/health-two.json"
list_one="$WORK_DIR/list-one.json"
list_two="$WORK_DIR/list-two.json"
add_response="$WORK_DIR/add.json"
db_failure_response="$WORK_DIR/db-failure.json"
db_recovery_response="$WORK_DIR/db-recovery.json"
storage_response="$WORK_DIR/storage.json"
cancel_response="$WORK_DIR/cancel.json"
cancel_storage_response="$WORK_DIR/cancel-storage.json"
probe_root="$WORK_DIR/data/fault-torrent"

cleanup() {
  if [[ -n "$daemon_pid" ]] && kill -0 "$daemon_pid" 2>/dev/null; then
    kill -TERM "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  if [[ -d "$WORK_DIR" ]]; then
    mkdir -p "$EVIDENCE_DIR"
    cp -a "$WORK_DIR/." "$EVIDENCE_DIR/"
    rm -rf "$WORK_DIR"
  fi
}
trap cleanup EXIT

if [[ -z "${TNG_FAULT_CONFIG:-}" ]]; then
  api_port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
  peer_port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
  sed \
    -e "s#127.0.0.1:28080#127.0.0.1:$api_port#" \
    -e "s#45555#$peer_port#" \
    -e "s#/tmp/torrentng-backend-burndown-release-smoke-session#$WORK_DIR/session#g" \
    -e "s#/tmp/torrentng-backend-burndown-release-smoke-data#$WORK_DIR/data#g" \
    "$BASE_CONFIG" >"$WORK_DIR/config.toml"
  config="$WORK_DIR/config.toml"
  url="http://127.0.0.1:$api_port"
  db_path="$WORK_DIR/session/state.db"
fi

if [[ -z "$db_path" ]]; then
  db_path="$(sed -n 's/^path = "\(.*\)"$/\1/p' "$config" | head -1)"
fi
test -n "$db_path"

auth_args=(-H "Authorization: Bearer $TOKEN")

start_daemon() {
  local log="$1"
  TORRENTNGD_CONFIG="$config" TNG_STATIC_DIR="$STATIC_DIR" \
    TNG_FAULT_STORAGE_DELAY_MS="$STORAGE_DELAY_MS" "$BIN" >"$log" 2>&1 &
  daemon_pid=$!
}

wait_ready() {
  local output="$1"
  for _ in $(seq 1 100); do
    local code
    code="$(curl -sS "${auth_args[@]}" -o "$output" -w '%{http_code}' "$url/health" 2>/dev/null || true)"
    if [[ "$code" == "200" ]] && python3 - "$output" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as handle:
    payload = json.load(handle)
raise SystemExit(0 if payload.get("ready") is True else 1)
PY
    then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

kill_daemon_hard() {
  if [[ -n "$daemon_pid" ]]; then
    kill -KILL "$daemon_pid"
    wait "$daemon_pid" 2>/dev/null || true
    daemon_pid=""
  fi
}

stop_daemon_clean() {
  if [[ -n "$daemon_pid" ]]; then
    kill -TERM "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
    daemon_pid=""
  fi
}

db_trigger_install() {
  python3 - "$db_path" <<'PY'
import sqlite3
import sys
db = sqlite3.connect(sys.argv[1], timeout=5)
db.execute("DROP TRIGGER IF EXISTS tng_fault_matrix_settings_fail")
db.execute("""
CREATE TRIGGER tng_fault_matrix_settings_fail
BEFORE INSERT ON settings
BEGIN
  SELECT RAISE(ABORT, 'injected live database failure');
END
""")
db.commit()
db.close()
PY
}

db_trigger_remove() {
  python3 - "$db_path" <<'PY'
import sqlite3
import sys
db = sqlite3.connect(sys.argv[1], timeout=5)
db.execute("DROP TRIGGER IF EXISTS tng_fault_matrix_settings_fail")
db.commit()
db.close()
PY
}

{
  echo "# TorrentNG Native Live Fault-Injection Matrix"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo "- Binary: $BIN"
  echo "- Config: $config"
  echo "- URL: $url"
  echo "- Database: $db_path"
  echo "- First storage-step cancellation delay: ${STORAGE_DELAY_MS}ms"
  echo "- Evidence artifacts: $EVIDENCE_DIR"
  echo
  echo "This test intentionally kills only the daemon started by this script."
  echo "It then exercises API cancellation, an external SQLite trigger, a filesystem"
  echo "failure, and the recovery path. It is not a throughput or 24-hour soak test."
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Evidence |"
  echo "|---|---|---|"
} >"$OUT"

start_daemon "$first_log"
wait_ready "$health_one"

probe_hash="0123456789abcdef0123456789abcdef01234567"
mkdir -p "$probe_root"
add_code="$(curl -sS "${auth_args[@]}" -H 'Content-Type: application/json' \
  -o "$add_response" -w '%{http_code}' -X POST "$url/api/v1/torrents" \
  --data "{\"magnet\":\"magnet:?xt=urn:btih:$probe_hash&dn=fault-probe\",\"save_path\":\"$probe_root\",\"start\":false}")"
if [[ "$add_code" != "201" ]]; then
  echo "add torrent failed with HTTP $add_code" >&2
  exit 1
fi
curl -sS "${auth_args[@]}" -o "$list_one" "$url/api/v1/torrents?limit=10" >/dev/null
python3 - "$list_one" "$probe_hash" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as handle:
    payload = json.load(handle)
hash_value = sys.argv[2]
if not any(item.get("info_hash") == hash_value for item in payload.get("torrents", [])):
    raise SystemExit("durable probe torrent was not listed before crash")
PY

kill_daemon_hard
start_daemon "$second_log"
wait_ready "$health_two"
curl -sS "${auth_args[@]}" -o "$list_two" "$url/api/v1/torrents?limit=10" >/dev/null
python3 - "$list_two" "$probe_hash" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as handle:
    payload = json.load(handle)
hash_value = sys.argv[2]
if not any(item.get("info_hash") == hash_value for item in payload.get("torrents", [])):
    raise SystemExit("durable probe torrent did not survive SIGKILL/restart")
PY

printf '| SIGKILL and durable restart recovery | PASS | %s, %s, %s |\n' \
  "$(basename "$first_log")" "$(basename "$second_log")" "$(basename "$list_two")" >>"$OUT"

db_trigger_install
db_failure_code="$(curl -sS "${auth_args[@]}" -H 'Content-Type: application/json' \
  -o "$db_failure_response" -w '%{http_code}' -X PUT "$url/api/v1/engine/user-agent" \
  --data '{"user_agent":"TorrentNG/fault-db"}')"
if [[ "$db_failure_code" != "400" ]]; then
  echo "expected injected DB mutation failure to return HTTP 400, got $db_failure_code" >&2
  exit 1
fi
if ! wait_ready "$health_two"; then
  echo "daemon did not remain healthy after injected DB failure" >&2
  exit 1
fi
db_trigger_remove
db_recovery_code="$(curl -sS "${auth_args[@]}" -H 'Content-Type: application/json' \
  -o "$db_recovery_response" -w '%{http_code}' -X PUT "$url/api/v1/engine/user-agent" \
  --data '{"user_agent":"TorrentNG/fault-db-recovered"}')"
if [[ "$db_recovery_code" != "204" ]]; then
  echo "DB worker did not recover after trigger removal; got HTTP $db_recovery_code" >&2
  exit 1
fi
printf '| Live SQLite failure crosses worker and recovers | PASS | %s, %s, health remains 200 |\n' \
  "$(basename "$db_failure_response")" "$(basename "$db_recovery_response")" >>"$OUT"

cancel_source="$WORK_DIR/data/cancel-source.bin"
cancel_destination="$WORK_DIR/data/cancel-destination.bin"
printf 'cancel-source' >"$cancel_source"
cancel_storage_code="$(curl -sS "${auth_args[@]}" -H 'Content-Type: application/json' \
  -o "$cancel_storage_response" -w '%{http_code}' -X POST "$url/api/v1/storage/execute" \
  --data "{\"operation\":\"import\",\"source\":\"$cancel_source\",\"destination\":\"$cancel_destination\",\"bytes\":13}")"
if [[ "$cancel_storage_code" != "202" ]]; then
  echo "cancellation storage job was not queued; got HTTP $cancel_storage_code" >&2
  exit 1
fi
cancel_job_id="$(python3 - "$cancel_storage_response" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as handle:
    payload = json.load(handle)
print(payload.get("job_id") or payload.get("job", {}).get("job_id") or "")
PY
)"
test -n "$cancel_job_id"
cancel_code="$(curl -sS "${auth_args[@]}" -o "$cancel_response" -w '%{http_code}' \
  -X POST "$url/api/v1/jobs/$cancel_job_id/cancel")"
if [[ "$cancel_code" != "204" ]]; then
  echo "live storage cancellation was not accepted; got HTTP $cancel_code" >&2
  exit 1
fi
cancel_state=""
for _ in $(seq 1 100); do
  cancel_state="$(python3 - "$db_path" "$cancel_job_id" <<'PY'
import sqlite3
import sys
db = sqlite3.connect(sys.argv[1], timeout=5)
row = db.execute("SELECT state FROM jobs WHERE job_id = ?", (sys.argv[2],)).fetchone()
print(row[0] if row else "")
db.close()
PY
)"
  if [[ "$cancel_state" == "cancelled" ]]; then
    break
  fi
  sleep 0.1
done
if [[ "$cancel_state" != "cancelled" || ! -f "$cancel_source" || -e "$cancel_destination" ]]; then
  echo "live cancellation did not leave a safe terminal state (state=$cancel_state)" >&2
  exit 1
fi
if ! wait_ready "$health_two"; then
  echo "daemon did not remain healthy after live cancellation" >&2
  exit 1
fi
printf '| Live storage cancellation is durable and isolated | PASS | %s; job %s cancelled, source retained, destination absent, health remains 200 |\n' \
  "$(basename "$cancel_response")" "$cancel_job_id" >>"$OUT"

source_path="$probe_root"
source_file="$source_path/fault-source.bin"
blocked_parent="$WORK_DIR/data/not-a-directory"
destination_path="$blocked_parent/fault-destination.bin"
printf 'fault-source' >"$source_file"
printf 'blocking-file' >"$blocked_parent"
storage_code="$(curl -sS "${auth_args[@]}" -H 'Content-Type: application/json' \
  -o "$storage_response" -w '%{http_code}' -X POST "$url/api/v1/storage/execute" \
  --data "{\"operation\":\"move\",\"source\":\"$source_path\",\"destination\":\"$destination_path\",\"bytes\":12,\"affected_torrents\":[\"$probe_hash\"]}")"
if [[ "$storage_code" != "202" ]]; then
  echo "storage failure probe was not queued; got HTTP $storage_code" >&2
  exit 1
fi
storage_job_id="$(python3 - "$storage_response" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as handle:
    payload = json.load(handle)
print(payload.get("job_id") or payload.get("job", {}).get("job_id") or "")
PY
)"
test -n "$storage_job_id"
storage_state=""
for _ in $(seq 1 100); do
  storage_state="$(python3 - "$db_path" "$storage_job_id" <<'PY'
import sqlite3
import sys
db = sqlite3.connect(sys.argv[1], timeout=5)
row = db.execute("SELECT state FROM jobs WHERE job_id = ?", (sys.argv[2],)).fetchone()
print(row[0] if row else "")
db.close()
PY
)"
  if [[ "$storage_state" == "failed" ]]; then
    break
  fi
  sleep 0.1
done
if [[ "$storage_state" != "failed" || ! -d "$source_path" || ! -f "$source_file" || -e "$destination_path" ]]; then
  echo "storage worker failure probe did not fail safely (state=$storage_state)" >&2
  exit 1
fi
if ! wait_ready "$health_two"; then
  echo "daemon did not remain healthy after storage worker failure" >&2
  exit 1
fi
printf '| Live filesystem failure is isolated and durable | PASS | %s; job %s failed, source retained, health remains 200 |\n' \
  "$(basename "$storage_response")" "$storage_job_id" >>"$OUT"

stop_daemon_clean

{
  echo
  echo "Overall status: PASS"
  echo
  echo "The matrix passed crash/restart, API cancellation, injected SQLite"
  echo "failure/recovery, and isolated storage failure. Logs and JSON probes were copied to"
  echo "$EVIDENCE_DIR."
} >>"$OUT"

echo "$OUT"
