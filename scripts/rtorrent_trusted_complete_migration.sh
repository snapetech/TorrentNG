#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/curl_policy.sh
source "$SCRIPT_DIR/curl_policy.sh"

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

Required environment:
  TNG_API_TOKEN               bearer token; kept out of command-line arguments

The script requires Python 3.11+ or the tomli module to validate its TOML config.

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
API_TOKEN="${TNG_API_TOKEN:-}"
unset TNG_API_TOKEN
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

path_is_within() {
  local candidate="${1%/}" parent="${2%/}"
  [[ -n "$candidate" ]] || candidate="/"
  [[ -n "$parent" ]] || parent="/"
  if [[ "$parent" == "/" ]]; then
    [[ "$candidate" == /* ]]
  else
    [[ "$candidate" == "$parent" || "$candidate" == "$parent/"* ]]
  fi
}

paths_overlap() {
  path_is_within "$1" "$2" || path_is_within "$2" "$1"
}

is_unsafe_data_root() {
  local candidate="$1" home_path="${HOME:-}"
  case "$candidate" in
    /|/home|/root|/var|/var/lib|/var/log|/var/cache|/var/tmp|/var/backups|/usr|/usr/local|/etc|/opt|/tmp|/mnt|/media|/srv|/proc|/sys|/dev|/run|/boot|/bin|/sbin|/lib|/lib64)
      return 0
      ;;
    /usr/*|/etc/*|/opt/*|/root/*|/var/log/*|/var/cache/*|/var/tmp/*|/var/spool/*|/proc/*|/sys/*|/dev/*|/run/*|/boot/*|/bin/*|/sbin/*|/lib/*|/lib64/*)
      return 0
      ;;
  esac
  if [[ "$candidate" == /var/lib/* ]]; then
    local varlib_app="${candidate#/var/lib/}"
    varlib_app="${varlib_app%%/*}"
    case "$varlib_app" in
      torrentng|torrentngd|rtorrent) ;;
      *) return 0 ;;
    esac
  fi
  case "$candidate" in
    /tmp/*)
      local tmp_relative="${candidate#/tmp/}"
      [[ "$tmp_relative" == */* ]] || return 0
      ;;
    /mnt/*)
      local mnt_relative="${candidate#/mnt/}"
      [[ "$mnt_relative" == */* ]] || return 0
      ;;
    /media/*)
      local media_relative="${candidate#/media/}"
      [[ "$media_relative" == */* ]] || return 0
      ;;
    /srv/*)
      local srv_relative="${candidate#/srv/}"
      [[ "$srv_relative" == */* ]] || return 0
      ;;
  esac
  if [[ "$candidate" == /home/* ]]; then
    local home_relative="${candidate#/home/}"
    [[ "$home_relative" == */* ]] || return 0
  fi
  if [[ -n "$home_path" ]]; then
    home_path="$(realpath -m -- "$home_path" 2>/dev/null || true)"
    if [[ -n "$home_path" ]] && path_is_within "$home_path" "$candidate"; then
      return 0
    fi
  fi
  return 1
}

canonical_existing_directory() {
  local value="$1" label="$2" resolved
  if [[ ! -d "$value" || -L "$value" ]]; then
    echo "$label must be an existing, non-symlink directory: $value" >&2
    return 1
  fi
  resolved="$(realpath -e -- "$value")" || {
    echo "cannot resolve $label: $value" >&2
    return 1
  }
  if is_unsafe_data_root "$resolved"; then
    echo "refusing broad $label path: $resolved" >&2
    return 1
  fi
  printf '%s' "$resolved"
}

reject_path_overlap() {
  local first="$1" first_label="$2" second="$3" second_label="$4"
  if paths_overlap "$first" "$second"; then
    echo "refusing overlapping paths: $first_label ($first) and $second_label ($second)" >&2
    return 1
  fi
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
  mkdir -p -- "$dest"
  if [[ -d "$src" ]]; then
    rsync -a --delete -- "$src"/ "$dest"/
  fi
}

backup_manifest() {
  local root="$1" out="$2"
  (cd "$root" && find . -type f -print0 | sort -z | xargs -0 -r sha256sum) >"$out"
}

restore_torrentngd() {
  local backup="$1" resolved
  log "restoring TorrentNG session from $backup"
  [[ -d "$TORRENTNGD_SESSION_DIR" && ! -L "$TORRENTNGD_SESSION_DIR" ]] || {
    echo "refusing to restore into a missing or symlinked TorrentNG session directory" >&2
    return 1
  }
  resolved="$(realpath -e -- "$TORRENTNGD_SESSION_DIR")" || return 1
  [[ "$resolved" == "$TORRENTNGD_SESSION_DIR" ]] || {
    echo "refusing to restore after TorrentNG session path changed: $resolved" >&2
    return 1
  }
  rm -rf -- "$resolved"
  mkdir -p -- "$resolved"
  rsync -a -- "$backup"/ "$resolved"/
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
need "$API_TOKEN" "TNG_API_TOKEN environment variable"

require_cmd docker
require_cmd rsync
require_cmd jq
require_cmd curl
require_cmd sha256sum
require_cmd python3
require_cmd realpath
require_cmd mktemp
require_cmd flock
require_cmd "$TORRENTNGD_BIN"

[[ -f "$COMPOSE_FILE" ]] || { echo "compose file not found: $COMPOSE_FILE" >&2; exit 2; }
[[ -d "$RTORRENT_SESSION_DIR" ]] || { echo "rTorrent session dir not found: $RTORRENT_SESSION_DIR" >&2; exit 2; }
[[ -d "$RTORRENT_CONFIG_DIR" ]] || { echo "rTorrent config dir not found: $RTORRENT_CONFIG_DIR" >&2; exit 2; }
[[ -f "$TORRENTNGD_CONFIG" ]] || { echo "TorrentNG config not found: $TORRENTNGD_CONFIG" >&2; exit 2; }
if [[ -n "$RTORRENT_WATCH_DIR" && ! -d "$RTORRENT_WATCH_DIR" ]]; then
  echo "rTorrent watch dir not found: $RTORRENT_WATCH_DIR" >&2
  exit 2
fi

[[ "$RTORRENT_SERVICE" =~ ^[A-Za-z0-9][A-Za-z0-9_.-]{0,62}$ ]] || { echo "invalid rTorrent Compose service name" >&2; exit 2; }
[[ "$TORRENTNGD_SERVICE" =~ ^[A-Za-z0-9][A-Za-z0-9_.-]{0,62}$ ]] || { echo "invalid TorrentNG Compose service name" >&2; exit 2; }

RTORRENT_SESSION_DIR="$(canonical_existing_directory "$RTORRENT_SESSION_DIR" "rTorrent session directory")" || exit 2
RTORRENT_CONFIG_DIR="$(canonical_existing_directory "$RTORRENT_CONFIG_DIR" "rTorrent config directory")" || exit 2
TORRENTNGD_SESSION_DIR="$(canonical_existing_directory "$TORRENTNGD_SESSION_DIR" "TorrentNG session directory")" || exit 2
if [[ -n "$RTORRENT_WATCH_DIR" ]]; then
  RTORRENT_WATCH_DIR="$(canonical_existing_directory "$RTORRENT_WATCH_DIR" "rTorrent watch directory")" || exit 2
fi
COMPOSE_FILE="$(realpath -e -- "$COMPOSE_FILE")" || { echo "cannot resolve Compose file" >&2; exit 2; }
TORRENTNGD_CONFIG="$(realpath -e -- "$TORRENTNGD_CONFIG")" || { echo "cannot resolve TorrentNG config" >&2; exit 2; }
python3 "$SCRIPT_DIR/validate_migration_config.py" "$TORRENTNGD_CONFIG" "$TORRENTNGD_SESSION_DIR"
python3 "$SCRIPT_DIR/protected_target.py" "$API_URL"

REPOSITORY_ROOT="$(realpath -e -- "$SCRIPT_DIR/..")"
if path_is_within "$TORRENTNGD_SESSION_DIR" "$REPOSITORY_ROOT"; then
  echo "refusing to use a repository path as the TorrentNG session directory" >&2
  exit 2
fi
reject_path_overlap "$TORRENTNGD_SESSION_DIR" "TorrentNG session directory" \
  "$RTORRENT_SESSION_DIR" "rTorrent session directory" || exit 2
reject_path_overlap "$TORRENTNGD_SESSION_DIR" "TorrentNG session directory" \
  "$RTORRENT_CONFIG_DIR" "rTorrent config directory" || exit 2
if [[ -n "$RTORRENT_WATCH_DIR" ]]; then
  reject_path_overlap "$TORRENTNGD_SESSION_DIR" "TorrentNG session directory" \
    "$RTORRENT_WATCH_DIR" "rTorrent watch directory" || exit 2
fi
if path_is_within "$COMPOSE_FILE" "$TORRENTNGD_SESSION_DIR" || \
   path_is_within "$TORRENTNGD_CONFIG" "$TORRENTNGD_SESSION_DIR"; then
  echo "refusing a TorrentNG session directory that contains its Compose or config file" >&2
  exit 2
fi

BACKUP_DIR="$(realpath -m -- "$BACKUP_DIR")" || { echo "cannot resolve backup directory" >&2; exit 2; }
if is_unsafe_data_root "$BACKUP_DIR"; then
  echo "refusing broad backup directory: $BACKUP_DIR" >&2
  exit 2
fi
reject_path_overlap "$BACKUP_DIR" "backup directory" "$RTORRENT_SESSION_DIR" "rTorrent session directory" || exit 2
reject_path_overlap "$BACKUP_DIR" "backup directory" "$RTORRENT_CONFIG_DIR" "rTorrent config directory" || exit 2
reject_path_overlap "$BACKUP_DIR" "backup directory" "$TORRENTNGD_SESSION_DIR" "TorrentNG session directory" || exit 2
if [[ -n "$RTORRENT_WATCH_DIR" ]]; then
  reject_path_overlap "$BACKUP_DIR" "backup directory" "$RTORRENT_WATCH_DIR" "rTorrent watch directory" || exit 2
fi

VERIFY_DELAY="${RTORRENT_MIGRATION_VERIFY_DELAY_SECS:-5}"
[[ "$VERIFY_DELAY" =~ ^[0-9]{1,3}$ ]] && ((10#$VERIFY_DELAY <= 300)) || {
  echo "RTORRENT_MIGRATION_VERIFY_DELAY_SECS must be an integer between 0 and 300" >&2
  exit 2
}

mkdir -p -- "$BACKUP_DIR"
BACKUP_DIR="$(realpath -e -- "$BACKUP_DIR")" || { echo "cannot resolve backup directory" >&2; exit 2; }
if is_unsafe_data_root "$BACKUP_DIR"; then
  echo "refusing broad backup directory: $BACKUP_DIR" >&2
  exit 2
fi
reject_path_overlap "$BACKUP_DIR" "backup directory" "$RTORRENT_SESSION_DIR" "rTorrent session directory" || exit 2
reject_path_overlap "$BACKUP_DIR" "backup directory" "$RTORRENT_CONFIG_DIR" "rTorrent config directory" || exit 2
reject_path_overlap "$BACKUP_DIR" "backup directory" "$TORRENTNGD_SESSION_DIR" "TorrentNG session directory" || exit 2
if [[ -n "$RTORRENT_WATCH_DIR" ]]; then
  reject_path_overlap "$BACKUP_DIR" "backup directory" "$RTORRENT_WATCH_DIR" "rTorrent watch directory" || exit 2
fi
[[ -w "$BACKUP_DIR" ]] || { echo "backup dir is not writable: $BACKUP_DIR" >&2; exit 2; }
exec 9<"$BACKUP_DIR"
flock -n 9 || { echo "another rTorrent migration holds the backup-directory lock" >&2; exit 2; }

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="$(mktemp -d -- "$BACKUP_DIR/rtorrent-migration-$STAMP.XXXXXX")"
RT_BACKUP="$RUN_DIR/rtorrent"
TNG_BACKUP="$RUN_DIR/torrentngd-session"
STAGING="$RUN_DIR/staging-rtorrent-session"
REPORT_MD="$RUN_DIR/rtorrent-trusted-complete.md"
REPORT_JSON="$RUN_DIR/rtorrent-trusted-complete.json"
HASHES="$RUN_DIR/selected-hashes.txt"
SELECTED_PATHS="$RUN_DIR/selected-session-paths.nul"
ARCHIVE="$RUN_DIR/archived-active-rtorrent-entries"
mkdir -p -- "$RT_BACKUP" "$STAGING" "$ARCHIVE"

RTORRENT_WAS_RUNNING=0
TORRENTNGD_WAS_RUNNING=0
MIGRATION_APPLY_ATTEMPTED=0
MIGRATION_COMMITTED=0
service_running "$RTORRENT_SERVICE" && RTORRENT_WAS_RUNNING=1
service_running "$TORRENTNGD_SERVICE" && TORRENTNGD_WAS_RUNNING=1

restore_archived_rtorrent_entries() {
  local archived_path relative destination destination_parent resolved_parent
  [[ -d "$ARCHIVE" ]] || return 0
  while IFS= read -r -d '' archived_path; do
    relative="${archived_path#"$ARCHIVE"/}"
    destination="$RTORRENT_SESSION_DIR/$relative"
    destination_parent="$(dirname -- "$destination")"
    mkdir -p -- "$destination_parent"
    resolved_parent="$(realpath -e -- "$destination_parent")" || return 1
    [[ "$resolved_parent" == "$destination_parent" ]] || {
      log "cannot restore archived entry through a changed directory: $relative"
      return 1
    }
    if [[ -e "$destination" || -L "$destination" ]]; then
      log "cannot restore archived entry because its original path exists: $relative"
      return 1
    fi
    mv -- "$archived_path" "$destination" || return 1
  done < <(find "$ARCHIVE" -type f -print0)
}

cleanup_migration() {
  local original_status=$? tng_restore_failed=0 rtorrent_restore_failed=0
  trap - EXIT
  if ((original_status != 0)) && ((MIGRATION_COMMITTED == 0)); then
    if ((MIGRATION_APPLY_ATTEMPTED != 0)); then
      log "migration failed before commit; restoring the pre-import TorrentNG session"
      if compose stop -t 60 "$TORRENTNGD_SERVICE"; then
        restore_torrentngd "$TNG_BACKUP" || tng_restore_failed=1
      else
        log "could not stop TorrentNG safely for rollback"
        tng_restore_failed=1
      fi
      if compose stop -t 60 "$RTORRENT_SERVICE"; then
        restore_archived_rtorrent_entries || rtorrent_restore_failed=1
      else
        log "could not stop rTorrent safely for rollback"
        rtorrent_restore_failed=1
      fi
    else
      restore_archived_rtorrent_entries || rtorrent_restore_failed=1
    fi
    if ((tng_restore_failed == 0)); then
      start_if_was_running "$TORRENTNGD_SERVICE" "$TORRENTNGD_WAS_RUNNING" || \
        log "failed to restore TorrentNG's original running state"
    fi
    if ((rtorrent_restore_failed == 0)); then
      start_if_was_running "$RTORRENT_SERVICE" "$RTORRENT_WAS_RUNNING" || \
        log "failed to restore rTorrent's original running state"
    fi
  fi
  exit "$original_status"
}
trap cleanup_migration EXIT

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
(cd "$RT_BACKUP" && sha256sum -c "$RUN_DIR/rtorrent-backup.sha256") >/dev/null

log "stopping TorrentNG and backing up TorrentNG-client session"
compose stop -t 60 "$TORRENTNGD_SERVICE"
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
python3 "$SCRIPT_DIR/stage_rtorrent_migration.py" \
  "$REPORT_JSON" "$RT_BACKUP/session" "$STAGING" "$SELECTED_PATHS"

if [[ "$DRY_RUN" == "1" ]]; then
  log "dry-run complete; restoring service state without changing active session"
  start_if_was_running "$TORRENTNGD_SERVICE" "$TORRENTNGD_WAS_RUNNING"
  start_if_was_running "$RTORRENT_SERVICE" "$RTORRENT_WAS_RUNNING"
  log "selected hashes: $HASHES"
  exit 0
fi

if [[ "$YES" != "1" ]]; then
  echo "refusing to apply without --yes after backup and staging" >&2
  echo "backup: $RUN_DIR" >&2
  exit 2
fi

log "applying TorrentNG import from filtered staging session"
MIGRATION_APPLY_ATTEMPTED=1
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
sleep "$VERIFY_DELAY"

FAILED=0
while IFS= read -r hash; do
  [[ -n "$hash" ]] || continue
  if ! verify_hash "$hash"; then
    echo "$hash" >>"$RUN_DIR/failed-verification.txt"
    FAILED=1
  fi
done <"$HASHES"

if [[ "$FAILED" == "1" ]]; then
  log "verification failed; rollback will restore the pre-import session"
  exit 1
fi

log "archiving migrated rTorrent session entries before rTorrent restart"
while IFS= read -r -d '' backup_path; do
  if ! path_is_within "$backup_path" "$RT_BACKUP/session" || \
     [[ "$backup_path" == "$RT_BACKUP/session" ]]; then
    echo "refusing to archive a path outside the rTorrent snapshot" >&2
    exit 1
  fi
  relative="${backup_path#"$RT_BACKUP/session"/}"
  source_path="$RTORRENT_SESSION_DIR/$relative"
  resolved_source="$(realpath -e -- "$source_path")" || {
    echo "selected rTorrent session entry disappeared: $relative" >&2
    exit 1
  }
  if [[ "$resolved_source" != "$source_path" || ! -f "$source_path" || -L "$source_path" ]]; then
    echo "refusing to archive a changed or non-regular rTorrent session entry: $relative" >&2
    exit 1
  fi
  archive_path="$ARCHIVE/$relative"
  mkdir -p -- "$(dirname -- "$archive_path")"
  [[ ! -e "$archive_path" && ! -L "$archive_path" ]] || {
    echo "archive destination already exists: $relative" >&2
    exit 1
  }
  mv -- "$source_path" "$archive_path"
done <"$SELECTED_PATHS"

log "restoring rTorrent service state with unmigrated torrents only"
start_if_was_running "$RTORRENT_SERVICE" "$RTORRENT_WAS_RUNNING"
MIGRATION_COMMITTED=1

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
