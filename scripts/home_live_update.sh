#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="${TNG_LIVE_REPO_DIR:-$(cd -- "$SCRIPT_DIR/.." && pwd)}"
REMOTE="${TNG_LIVE_REMOTE:-origin}"
BRANCH="${TNG_LIVE_BRANCH:-main}"
COMPOSE_FILE="${TNG_LIVE_COMPOSE_FILE:-deploy/docker/compose.yml}"
COMPOSE_ENV_FILE="${TNG_LIVE_COMPOSE_ENV_FILE:-}"
SERVICE="${TNG_LIVE_SERVICE:-torrentng}"
LOCK_FILE="${TNG_LIVE_LOCK_FILE:-/tmp/torrentng-live-main-update.lock}"
ALLOW_DIRTY="${TNG_LIVE_ALLOW_DIRTY:-0}"
DRY_RUN="${TNG_LIVE_DRY_RUN:-0}"
FORCE="${TNG_LIVE_FORCE:-0}"
PRUNE="${TNG_LIVE_PRUNE:-0}"

log() {
  printf '[TorrentNG live] %s\n' "$*"
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

compose_cmd=(docker compose)
if [[ -n "$COMPOSE_ENV_FILE" ]]; then
  if [[ ! -f "$COMPOSE_ENV_FILE" ]]; then
    log "compose env file not found: $COMPOSE_ENV_FILE"
    exit 2
  fi
  compose_cmd+=(--env-file "$COMPOSE_ENV_FILE")
fi
compose_cmd+=(-f "$COMPOSE_FILE")

if [[ "$ALLOW_DIRTY" != "1" ]] && [[ -n "$(git status --porcelain)" ]]; then
  log "worktree has local changes; refusing to auto-update"
  log "commit/stash the changes, or set TNG_LIVE_ALLOW_DIRTY=1 for a disposable checkout"
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

up_args=(up -d --no-deps)
if [[ "$FORCE" == "1" ]]; then
  up_args+=(--force-recreate)
fi

run "${compose_cmd[@]}" build "$SERVICE"
run "${compose_cmd[@]}" "${up_args[@]}" "$SERVICE"

if "${compose_cmd[@]}" config --services | grep -qx nginx; then
  run "${compose_cmd[@]}" "${up_args[@]}" nginx
fi

if [[ "$PRUNE" == "1" ]]; then
  run docker image prune -f
fi

log "updated $SERVICE live build to $new_head"
