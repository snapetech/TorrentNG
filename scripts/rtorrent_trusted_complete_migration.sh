#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/rtorrent_trusted_complete_migration.sh [OPTIONS]

Required:
  --compose-file PATH
  --rtorrent-service NAME
  --torrentngd-service NAME
  --rtorrent-session-dir DIR
  --rtorrent-config-dir DIR
  --torrentngd-session-dir DIR
  --torrentngd-config FILE
  --backup-dir DIR
  --api-url URL
  --api-token TOKEN

Optional:
  --rtorrent-watch-dir DIR
  --torrentngd-bin PATH       default: torrentngd
  --remap OLD=NEW             repeatable
  --yes                       required for non-dry-run archive/restart
  --dry-run                   stop after backup, dry-run report, and staging

This script migrates only rTorrent torrents that import as completed + Trusted.
It backs up rTorrent state before import, imports a filtered staging session into
TorrentNG, verifies each selected hash is listed complete/seeding, then archives
migrated rTorrent session entries before restarting rTorrent.
USAGE
}

COMPOSE_FILE=""
RTORRENT_SERVICE=""
TORRENTNGD_SERVICE=""
RTORRENT_SESSION_DIR=""
RTORRENT_CONFIG_DIR=""
RTORRENT_WATCH_DIR=""
TORRENTNGD_SESSION_DIR=""
TORRENTNGD_CONFIG=""
BACKUP_DIR=""
API_URL=""
API_TOKEN=""
TORRENTNGD_BIN="${TORRENTNGD_BIN:-torrentngd}"
YES=0
DRY_RUN=0
REMAPS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --compose-file) shift; COMPOSE_FILE="${1:?missing --compose-file value}" ;;
    --rtorrent-service) shift; RTORRENT_SERVICE="${1:?missing --rtorrent-service value}" ;;
    --torrentngd-service) shift; TORRENTNGD_SERVICE="${1:?missing --torrentngd-service value}" ;;
    --rtorrent-session-dir) shift; RTORRENT_SESSION_DIR="${1:?missing --rtorrent-session-dir value}" ;;
    --rtorrent-config-dir) shift; RTORRENT_CONFIG_DIR="${1:?missing --rtorrent-config-dir value}" ;;
    --rtorrent-watch-dir) shift; RTORRENT_WATCH_DIR="${1:?missing --rtorrent-watch-dir value}" ;;
    --torrentngd-session-dir) shift; TORRENTNGD_SESSION_DIR="${1:?missing --torrentngd-session-dir value}" ;;
    --torrentngd-config) shift; TORRENTNGD_CONFIG="${1:?missing --torrentngd-config value}" ;;
    --backup-dir) shift; BACKUP_DIR="${1:?missing --backup-dir value}" ;;
    --api-url) shift; API_URL="${1:?missing --api-url value}" ;;
    --api-token) shift; API_TOKEN="${1:?missing --api-token value}" ;;
    --torrentngd-bin) shift; TORRENTNGD_BIN="${1:?missing --torrentngd-bin value}" ;;
    --remap) shift; REMAPS+=("--remap" "${1:?missing --remap value}") ;;
    --yes|-y) YES=1 ;;
    --dry-run) DRY_RUN=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

need() {
  local value="$1" name="$2"
  [[ -n "$value" ]] || { echo "missing required $name" >&2; exit 2; }
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || { echo "missing required command: $1" >&2; exit 127; }
}

compose() {
  docker compose -f "$COMPOSE_FILE" "$@"
}

service_running() {
  local service="$1" id
  id="$(compose ps -q "$service")"
  [[ -n "$id" ]] && [[ "$(docker inspect -f '{{.State.Running}}' "$id" 2>/dev/null || true)" == "true" ]]
}

start_if_was_running() {
  local service="$1" was_running="$2"
  if [[ "$was_running" == "1" ]]; then
    compose start "$service"
  fi
}

log() {
  printf '[rtorrent-migrate] %s\n' "$*" >&2
}

copy_dir() {
  local src="$1" dest="$2"
  mkdir -p "$dest"
  if [[ -d "$src" ]]; then
    rsync -a --delete "$src"/ "$dest"/
  fi
}

backup_manifest() {
  local root="$1" out="$2"
  (cd "$root" && find . -type f -print0 | sort -z | xargs -0 sha256sum) >"$out"
}

restore_torrentngd() {
  local backup="$1"
  log "restoring TorrentNG session from $backup"
  rm -rf "$TORRENTNGD_SESSION_DIR"
  mkdir -p "$TORRENTNGD_SESSION_DIR"
  rsync -a "$backup"/ "$TORRENTNGD_SESSION_DIR"/
}

verify_hash() {
  local hash="$1" body
  body="$(curl -fsS -H "Authorization: Bearer $API_TOKEN" "$API_URL/api/v1/torrents")"
  jq -e --arg hash "$hash" '
    map(select((.info_hash // .hash // .hash_string // "") == $hash))
    | any(
        ((.state // .status // "") | tostring | ascii_downcase | test("seed|complete"))
        or ((.progress // .percent_complete // 0) == 1)
      )
  ' <<<"$body" >/dev/null
}

need "$COMPOSE_FILE" "--compose-file"
need "$RTORRENT_SERVICE" "--rtorrent-service"
need "$TORRENTNGD_SERVICE" "--torrentngd-service"
need "$RTORRENT_SESSION_DIR" "--rtorrent-session-dir"
need "$RTORRENT_CONFIG_DIR" "--rtorrent-config-dir"
need "$TORRENTNGD_SESSION_DIR" "--torrentngd-session-dir"
need "$TORRENTNGD_CONFIG" "--torrentngd-config"
need "$BACKUP_DIR" "--backup-dir"
need "$API_URL" "--api-url"
need "$API_TOKEN" "--api-token"

require_cmd docker
require_cmd rsync
require_cmd jq
require_cmd curl
require_cmd sha256sum
require_cmd "$TORRENTNGD_BIN"

[[ -f "$COMPOSE_FILE" ]] || { echo "compose file not found: $COMPOSE_FILE" >&2; exit 2; }
[[ -d "$RTORRENT_SESSION_DIR" ]] || { echo "rTorrent session dir not found: $RTORRENT_SESSION_DIR" >&2; exit 2; }
[[ -d "$RTORRENT_CONFIG_DIR" ]] || { echo "rTorrent config dir not found: $RTORRENT_CONFIG_DIR" >&2; exit 2; }
[[ -d "$TORRENTNGD_SESSION_DIR" ]] || { echo "TorrentNG session dir not found: $TORRENTNGD_SESSION_DIR" >&2; exit 2; }
[[ -f "$TORRENTNGD_CONFIG" ]] || { echo "TorrentNG config not found: $TORRENTNGD_CONFIG" >&2; exit 2; }
mkdir -p "$BACKUP_DIR"
[[ -w "$BACKUP_DIR" ]] || { echo "backup dir is not writable: $BACKUP_DIR" >&2; exit 2; }

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="$BACKUP_DIR/rtorrent-migration-$STAMP"
RT_BACKUP="$RUN_DIR/rtorrent"
TNG_BACKUP="$RUN_DIR/torrentngd-session"
STAGING="$RUN_DIR/staging-rtorrent-session"
REPORT_MD="$RUN_DIR/rtorrent-trusted-complete.md"
REPORT_JSON="$RUN_DIR/rtorrent-trusted-complete.json"
HASHES="$RUN_DIR/selected-hashes.txt"
SELECTED_PATHS="$RUN_DIR/selected-session-paths.tsv"
ARCHIVE="$RUN_DIR/archived-active-rtorrent-entries"
mkdir -p "$RT_BACKUP" "$STAGING" "$ARCHIVE"

RTORRENT_WAS_RUNNING=0
TORRENTNGD_WAS_RUNNING=0
service_running "$RTORRENT_SERVICE" && RTORRENT_WAS_RUNNING=1
service_running "$TORRENTNGD_SERVICE" && TORRENTNGD_WAS_RUNNING=1

log "stopping rTorrent before backup"
compose stop -t 60 "$RTORRENT_SERVICE"

log "backing up rTorrent session and config to $RUN_DIR"
copy_dir "$RTORRENT_SESSION_DIR" "$RT_BACKUP/session"
copy_dir "$RTORRENT_CONFIG_DIR" "$RT_BACKUP/config"
if [[ -n "$RTORRENT_WATCH_DIR" ]]; then
  copy_dir "$RTORRENT_WATCH_DIR" "$RT_BACKUP/watch"
fi
cp "$COMPOSE_FILE" "$RT_BACKUP/compose.yml"
backup_manifest "$RT_BACKUP" "$RUN_DIR/rtorrent-backup.sha256"
sha256sum -c "$RUN_DIR/rtorrent-backup.sha256" >/dev/null

log "stopping TorrentNG and backing up native session"
compose stop -t 60 "$TORRENTNGD_SERVICE" || true
copy_dir "$TORRENTNGD_SESSION_DIR" "$TNG_BACKUP"

log "dry-running trusted completed rTorrent import from backup"
"$TORRENTNGD_BIN" migrate \
  --source rtorrent \
  --from "$RT_BACKUP/session" \
  --config "$TORRENTNGD_CONFIG" \
  --only-trusted \
  --only-complete \
  --report "$REPORT_MD" \
  --report-json "$REPORT_JSON" \
  "${REMAPS[@]}"

jq -r '.torrents[].info_hash' "$REPORT_JSON" >"$HASHES"
SELECTED_COUNT="$(wc -l <"$HASHES" | tr -d ' ')"
if [[ "$SELECTED_COUNT" == "0" ]]; then
  log "no trusted completed rTorrent torrents selected; restoring service state"
  start_if_was_running "$TORRENTNGD_SERVICE" "$TORRENTNGD_WAS_RUNNING"
  start_if_was_running "$RTORRENT_SERVICE" "$RTORRENT_WAS_RUNNING"
  exit 0
fi

log "staging $SELECTED_COUNT trusted completed torrent(s)"
jq -r '.torrents[] | [.torrent_path, (.resume_path // "")] | @tsv' "$REPORT_JSON" >"$SELECTED_PATHS"
while IFS=$'\t' read -r torrent_path resume_path; do
  cp "$torrent_path" "$STAGING/"
  if [[ -n "$resume_path" ]]; then
    cp "$resume_path" "$STAGING/"
  fi
done <"$SELECTED_PATHS"

if [[ "$DRY_RUN" == "1" ]]; then
  log "dry-run complete; restoring service state without changing active session"
  start_if_was_running "$TORRENTNGD_SERVICE" "$TORRENTNGD_WAS_RUNNING"
  start_if_was_running "$RTORRENT_SERVICE" "$RTORRENT_WAS_RUNNING"
  log "selected hashes: $HASHES"
  exit 0
fi

if [[ "$YES" != "1" ]]; then
  start_if_was_running "$TORRENTNGD_SERVICE" "$TORRENTNGD_WAS_RUNNING"
  start_if_was_running "$RTORRENT_SERVICE" "$RTORRENT_WAS_RUNNING"
  echo "refusing to apply without --yes after backup and staging" >&2
  echo "backup: $RUN_DIR" >&2
  exit 2
fi

log "applying TorrentNG import from filtered staging session"
"$TORRENTNGD_BIN" migrate \
  --source rtorrent \
  --from "$STAGING" \
  --config "$TORRENTNGD_CONFIG" \
  --policy trust-hints \
  --only-trusted \
  --only-complete \
  --apply \
  --yes \
  "${REMAPS[@]}"

log "starting TorrentNG for verification"
compose start "$TORRENTNGD_SERVICE"
sleep "${RTORRENT_MIGRATION_VERIFY_DELAY_SECS:-5}"

FAILED=0
while IFS= read -r hash; do
  [[ -n "$hash" ]] || continue
  if ! verify_hash "$hash"; then
    echo "$hash" >>"$RUN_DIR/failed-verification.txt"
    FAILED=1
  fi
done <"$HASHES"

if [[ "$FAILED" == "1" ]]; then
  log "verification failed; restoring TorrentNG and restarting rTorrent"
  compose stop -t 60 "$TORRENTNGD_SERVICE" || true
  restore_torrentngd "$TNG_BACKUP"
  start_if_was_running "$TORRENTNGD_SERVICE" "$TORRENTNGD_WAS_RUNNING"
  start_if_was_running "$RTORRENT_SERVICE" "$RTORRENT_WAS_RUNNING"
  exit 1
fi

log "archiving migrated rTorrent session entries before rTorrent restart"
while IFS=$'\t' read -r torrent_path resume_path; do
  for staged_path in "$torrent_path" "$resume_path"; do
    [[ -n "$staged_path" ]] || continue
    name="$(basename "$staged_path")"
    if [[ -e "$RTORRENT_SESSION_DIR/$name" ]]; then
      mv "$RTORRENT_SESSION_DIR/$name" "$ARCHIVE/"
    fi
  done
done <"$SELECTED_PATHS"

log "restoring rTorrent service state with unmigrated torrents only"
start_if_was_running "$RTORRENT_SERVICE" "$RTORRENT_WAS_RUNNING"

cat >"$RUN_DIR/summary.md" <<EOF
# rTorrent Trusted Complete Migration

- Result: PASS
- Selected torrents: $SELECTED_COUNT
- Backup: $RUN_DIR
- rTorrent backup manifest: $RUN_DIR/rtorrent-backup.sha256
- Dry-run report: $REPORT_MD
- JSON report: $REPORT_JSON
- Archived migrated entries: $ARCHIVE
EOF

log "migration complete: $RUN_DIR/summary.md"
