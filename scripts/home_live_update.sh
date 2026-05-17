#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="${RTNG_LIVE_REPO_DIR:-$(cd -- "$SCRIPT_DIR/.." && pwd)}"
REMOTE="${RTNG_LIVE_REMOTE:-origin}"
BRANCH="${RTNG_LIVE_BRANCH:-main}"
COMPOSE_FILE="${RTNG_LIVE_COMPOSE_FILE:-deploy/docker/compose.yml}"
SERVICE="${RTNG_LIVE_SERVICE:-rtorrentng}"
LOCK_FILE="${RTNG_LIVE_LOCK_FILE:-/tmp/rtorrentng-live-main-update.lock}"
ALLOW_DIRTY="${RTNG_LIVE_ALLOW_DIRTY:-0}"
DRY_RUN="${RTNG_LIVE_DRY_RUN:-0}"
FORCE="${RTNG_LIVE_FORCE:-0}"
PRUNE="${RTNG_LIVE_PRUNE:-0}"

log() {
  printf '[rtorrentNG live] %s\n' "$*"
}

run() {
  log "+ $*"
  if [[ "$DRY_RUN" != "1" ]]; then
    "$@"
  fi
}

exec 9>"$LOCK_FILE"
if ! flock -n 9; then
  log "another live update is already running"
  exit 0
fi

cd "$REPO_DIR"

if [[ ! -d .git ]]; then
  log "$REPO_DIR is not a git repository"
  exit 2
fi

if [[ ! -f "$COMPOSE_FILE" ]]; then
  log "compose file not found: $COMPOSE_FILE"
  exit 2
fi

if [[ "$ALLOW_DIRTY" != "1" ]] && [[ -n "$(git status --porcelain)" ]]; then
  log "worktree has local changes; refusing to auto-update"
  log "commit/stash the changes, or set RTNG_LIVE_ALLOW_DIRTY=1 for a disposable checkout"
  exit 3
fi

current_head="$(git rev-parse HEAD)"

run git fetch --prune "$REMOTE" "$BRANCH"
target_head="$(git rev-parse "$REMOTE/$BRANCH")"

if [[ "$current_head" == "$target_head" && "$FORCE" != "1" ]]; then
  log "already at $BRANCH ($target_head); no rebuild needed"
  exit 0
fi

run git checkout "$BRANCH"
run git pull --ff-only "$REMOTE" "$BRANCH"

new_head="$(git rev-parse HEAD)"
if [[ "$current_head" == "$new_head" && "$FORCE" != "1" ]]; then
  log "no new commit after pull; no rebuild needed"
  exit 0
fi

run docker compose -f "$COMPOSE_FILE" build "$SERVICE"
run docker compose -f "$COMPOSE_FILE" up -d --no-deps "$SERVICE"

if docker compose -f "$COMPOSE_FILE" config --services | grep -qx nginx; then
  run docker compose -f "$COMPOSE_FILE" up -d --no-deps nginx
fi

if [[ "$PRUNE" == "1" ]]; then
  run docker image prune -f
fi

log "updated $SERVICE live build to $new_head"
