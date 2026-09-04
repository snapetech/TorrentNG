#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
BIN="${TNG_BACKUP_BINARY:-$ROOT/target/release/torrentngd}"
STATIC_DIR="${TNG_STATIC_DIR:-$ROOT/sidecar/static}"
TOKEN="${TNG_BACKUP_TOKEN:-backup-restore-cert-token-20260904}"
FIXTURE="${TNG_BACKUP_FIXTURE:-$ROOT/certification/interop/torrents/rust-restart-recovery.torrent}"
OUT="${1:-$REPORT_DIR/backup-restore-certification-$(date -u +%Y%m%dT%H%M%SZ).md}"
PREFIX="${OUT%.md}"

mkdir -p "$(dirname "$OUT")"
test -x "$BIN"
test -f "$FIXTURE"
test -d "$STATIC_DIR"
command -v curl >/dev/null
command -v jq >/dev/null
command -v sqlite3 >/dev/null
command -v tar >/dev/null

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/torrentng-backup-restore.XXXXXX")"
SOURCE_SESSION="$WORK_DIR/source-session"
SOURCE_DATA="$WORK_DIR/source-data"
BACKUP_ROOT="$WORK_DIR/backup"
RESTORE_ROOT="$WORK_DIR/restore"
SOURCE_CONFIG="$WORK_DIR/source-config.toml"
RESTORE_CONFIG="$RESTORE_ROOT/restored-config.toml"
SOURCE_LOG="$PREFIX.source.log"
RESTORE_LOG="$PREFIX.restore.log"
ARCHIVE="$WORK_DIR/torrentng-session.tar.gz"
SOURCE_LIST="$WORK_DIR/source-list.json"
RESTORED_LIST="$WORK_DIR/restored-list.json"
ADD_BODY="$WORK_DIR/add.json"
daemon_pid=""
status="PASS"
failure_reason=""
finalized=0

write_header() {
  cat >"$OUT" <<EOF
# TorrentNG Backup/Restore Certification

- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)
- Host: $(hostname)
- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)
- Binary: $BIN
- Binary SHA-256: $(sha256sum "$BIN" | awk '{print $1}')
- Fixture: $FIXTURE

This is a disposable native-engine drill. It creates one paused torrent in a
temporary session, performs a SQLite online backup plus session archive, tears
down the source daemon, restores the archive into a different session/data
root, starts a second daemon, and verifies that the restored state is usable.
Payload bytes are intentionally not part of this drill; production payloads
remain a separate storage-backup responsibility.

## Checks

| Check | Result | Detail |
|---|---|---|
EOF
}

record() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" >>"$OUT"
}

finalize() {
  [[ "$finalized" == "1" ]] && return 0
  finalized=1
  if [[ "$status" == "PASS" ]]; then
    {
      echo
      echo "- Source session: one paused torrent with persisted category and tag state"
      echo "- Restored session: state listed, then a category mutation committed"
      echo "- Archive bytes: $(stat -c '%s' "$ARCHIVE" 2>/dev/null || echo unavailable)"
      echo "- Source SQLite integrity: ${source_integrity:-not reached}"
      echo "- Restored SQLite integrity: ${restored_integrity:-not reached}"
      echo
      echo "Overall status: PASS"
    } >>"$OUT"
  else
    {
      echo
      echo "- Failure: ${failure_reason:-unknown failure}"
      echo
      echo "Overall status: FAIL"
    } >>"$OUT"
  fi
  echo "$OUT"
}

stop_daemon() {
  if [[ -n "$daemon_pid" ]] && kill -0 "$daemon_pid" 2>/dev/null; then
    kill -TERM "$daemon_pid" 2>/dev/null || true
    for _ in $(seq 1 60); do
      if ! kill -0 "$daemon_pid" 2>/dev/null; then
        break
      fi
      sleep 0.1
    done
    wait "$daemon_pid" 2>/dev/null || true
  fi
  daemon_pid=""
}

cleanup() {
  local exit_code=$?
  stop_daemon
  if [[ "$status" == "PASS" && "$exit_code" -ne 0 ]]; then
    status="FAIL"
    failure_reason="command failed with exit $exit_code"
  fi
  finalize
  rm -f "$SOURCE_LOG" "$RESTORE_LOG"
  rm -rf "$WORK_DIR"
  exit "$exit_code"
}

on_error() {
  status="FAIL"
  failure_reason="unexpected failure near line $1"
  return 1
}

trap 'on_error "$LINENO"' ERR
trap cleanup EXIT

write_header
mkdir -p "$SOURCE_SESSION" "$SOURCE_DATA" "$BACKUP_ROOT/session" "$RESTORE_ROOT"

api_port="${TNG_BACKUP_API_PORT:-$((28080 + (BASHPID % 900)))}"
while curl -fsS --max-time 0.1 "http://127.0.0.1:$api_port/health" >/dev/null 2>&1; do
  api_port=$((api_port + 1))
done
restore_port=$((api_port + 1))
while curl -fsS --max-time 0.1 "http://127.0.0.1:$restore_port/health" >/dev/null 2>&1; do
  restore_port=$((restore_port + 1))
done

cat >"$SOURCE_CONFIG" <<EOF
[daemon]
api_bind = "127.0.0.1:$api_port"
session_dir = "$SOURCE_SESSION"
shutdown_timeout_secs = 10

[network]
listen_port = 0
max_peers = 8

[storage]
download_dir = "$SOURCE_DATA"

[runtime]
torrent_tiers_enabled = true

[dht]
enabled = false

[db]
path = "$SOURCE_SESSION/state.db"

[auth]
api_tokens = ["$TOKEN"]
EOF

start_daemon() {
  local config="$1"
  local log="$2"
  local port="$3"
  TORRENTNGD_CONFIG="$config" TNG_STATIC_DIR="$STATIC_DIR" "$BIN" >"$log" 2>&1 &
  daemon_pid=$!
  for _ in $(seq 1 100); do
    if curl -fsS -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$port/health" >/dev/null 2>&1; then
      return 0
    fi
    if ! kill -0 "$daemon_pid" 2>/dev/null; then
      return 1
    fi
    sleep 0.1
  done
  return 1
}

start_daemon "$SOURCE_CONFIG" "$SOURCE_LOG" "$api_port"
record "Source daemon startup and health" "PASS" "authenticated native health endpoint"

torrent_b64="$(base64 <"$FIXTURE" | tr -d '\n')"
request="$(jq -nc --arg torrent "$torrent_b64" --arg path "$SOURCE_DATA" '{torrent_b64:$torrent,save_path:$path,start:false,category:"backup-cert",tags:["backup-restore"]}')"
add_code="$(curl -fsS -o "$ADD_BODY" -w '%{http_code}' \
  -X POST -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  "http://127.0.0.1:$api_port/api/v1/torrents" -d "$request")"
[[ "$add_code" == "201" ]]
info_hash="$(jq -er '.info_hash' "$ADD_BODY")"
record "Persisted torrent creation" "PASS" "hash=$info_hash"

curl -fsS -H "Authorization: Bearer $TOKEN" \
  "http://127.0.0.1:$api_port/api/v1/torrents?limit=10" >"$SOURCE_LIST"
jq -e --arg hash "$info_hash" '
  .total == 1 and (.torrents | length == 1) and
  .torrents[0].info_hash == $hash and
  .torrents[0].category == "backup-cert" and
  (.torrents[0].tags | index("backup-restore") != null)
' "$SOURCE_LIST" >/dev/null
record "Source state projection" "PASS" "category and tag survived API read"

stop_daemon
source_integrity="$(sqlite3 "$SOURCE_SESSION/state.db" 'PRAGMA integrity_check;')"
[[ "$source_integrity" == "ok" ]]
record "Source SQLite integrity" "PASS" "PRAGMA integrity_check=ok"

cp "$SOURCE_CONFIG" "$BACKUP_ROOT/config.toml"
while IFS= read -r -d '' item; do
  name="$(basename "$item")"
  case "$name" in
    state.db|state.db-wal|state.db-shm) continue ;;
  esac
  cp -a "$item" "$BACKUP_ROOT/session/"
done < <(find "$SOURCE_SESSION" -mindepth 1 -maxdepth 1 -print0)
sqlite3 "$SOURCE_SESSION/state.db" ".backup '$BACKUP_ROOT/session/state.db'"
tar -C "$BACKUP_ROOT" -czf "$ARCHIVE" config.toml session
tar -tzf "$ARCHIVE" >/dev/null
record "Archive creation and listing" "PASS" "SQLite online backup plus session metadata archive"

tar -xzf "$ARCHIVE" -C "$RESTORE_ROOT"
restore_session="$RESTORE_ROOT/session"
restore_data="$RESTORE_ROOT/data"
mkdir -p "$restore_data"
sed \
  -e "s|$SOURCE_SESSION|$restore_session|g" \
  -e "s|$SOURCE_DATA|$restore_data|g" \
  -e "s|127.0.0.1:$api_port|127.0.0.1:$restore_port|g" \
  "$RESTORE_ROOT/config.toml" >"$RESTORE_CONFIG"

restored_integrity="$(sqlite3 "$restore_session/state.db" 'PRAGMA integrity_check;')"
[[ "$restored_integrity" == "ok" ]]
record "Restored SQLite integrity" "PASS" "PRAGMA integrity_check=ok"

start_daemon "$RESTORE_CONFIG" "$RESTORE_LOG" "$restore_port"
record "Restored daemon startup and health" "PASS" "restored config and session accepted"
curl -fsS -H "Authorization: Bearer $TOKEN" \
  "http://127.0.0.1:$restore_port/api/v1/torrents?limit=10" >"$RESTORED_LIST"
source_signature="$(jq -c '.torrents | map({info_hash,name,state,category,tags}) | sort_by(.info_hash)' "$SOURCE_LIST")"
restored_signature="$(jq -c '.torrents | map({info_hash,name,state,category,tags}) | sort_by(.info_hash)' "$RESTORED_LIST")"
[[ "$source_signature" == "$restored_signature" ]]
record "Restored torrent identity and metadata" "PASS" "hash, state, category, and tags match source"

mutation_code="$(curl -fsS -o /dev/null -w '%{http_code}' \
  -X PUT -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  "http://127.0.0.1:$restore_port/api/v1/torrents/$info_hash/category" \
  -d '{"category":"backup-restored"}')"
[[ "$mutation_code" == "204" ]]
category_updated=0
for _ in $(seq 1 20); do
  curl -fsS -H "Authorization: Bearer $TOKEN" \
    "http://127.0.0.1:$restore_port/api/v1/torrents?limit=10" >"$RESTORED_LIST"
  if jq -e '.torrents[0].category == "backup-restored"' "$RESTORED_LIST" >/dev/null; then
    category_updated=1
    break
  fi
  sleep 0.1
done
[[ "$category_updated" == "1" ]]
record "Post-restore mutation" "PASS" "category update committed through restored worker-backed DB"

stop_daemon
finalize
