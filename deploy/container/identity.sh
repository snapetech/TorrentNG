#!/bin/sh

# Shared runtime identity setup for the native and compatible-client images.
# The image starts this helper as root only long enough to prepare the small
# runtime directory roots, then re-execs Tini after dropping all capabilities.
# It deliberately does not recursively chown mounted download trees.

tng_identity_fail() {
  echo "TorrentNG container identity: $*" >&2
  exit 1
}

tng_identity_validate() {
  TNG_DEFAULT_UID=${TNG_DEFAULT_UID:-1000}
  TNG_DEFAULT_GID=${TNG_DEFAULT_GID:-1000}
  PUID=${PUID:-$TNG_DEFAULT_UID}
  PGID=${PGID:-$TNG_DEFAULT_GID}
  export PUID PGID

  case "$PUID" in
    ''|0*|*[!0-9]*) tng_identity_fail "PUID must be a nonzero decimal UID" ;;
  esac
  case "$PGID" in
    ''|0*|*[!0-9]*) tng_identity_fail "PGID must be a nonzero decimal GID" ;;
  esac
  [ -n "${TNG_IDENTITY_PATHS:-}" ] ||
    tng_identity_fail "TNG_IDENTITY_PATHS is not configured in the image"
}

tng_identity_prepare_paths() {
  for identity_path in $TNG_IDENTITY_PATHS; do
    case "$identity_path" in
      /*) ;;
      *) tng_identity_fail "identity path must be absolute: $identity_path" ;;
    esac
    mkdir -p "$identity_path" ||
      tng_identity_fail "cannot create identity path $identity_path"
    chown "$PUID:$PGID" "$identity_path" ||
      tng_identity_fail "cannot assign $PUID:$PGID to $identity_path"
  done
  for identity_path in ${TNG_IDENTITY_RECURSIVE_PATHS:-}; do
    case "$identity_path" in
      /*) ;;
      *) tng_identity_fail "recursive identity path must be absolute: $identity_path" ;;
    esac
    mkdir -p "$identity_path" ||
      tng_identity_fail "cannot create recursive identity path $identity_path"
    chown -R "$PUID:$PGID" "$identity_path" ||
      tng_identity_fail "cannot assign $PUID:$PGID recursively to $identity_path"
  done
}

tng_identity_check_nonroot() {
  actual_uid=$(id -u)
  actual_gid=$(id -g)
  if [ "$actual_uid" != "$PUID" ] || [ "$actual_gid" != "$PGID" ]; then
    tng_identity_fail "process is $actual_uid:$actual_gid but PUID/PGID is $PUID:$PGID; remove --user or set both variables to the started identity"
  fi

  for identity_path in $TNG_IDENTITY_PATHS; do
    [ -e "$identity_path" ] || continue
    [ -w "$identity_path" ] ||
      tng_identity_fail "identity path is not writable as $PUID:$PGID: $identity_path; start without a --user override so the entrypoint can prepare it"
  done
  for identity_path in ${TNG_IDENTITY_RECURSIVE_PATHS:-}; do
    [ -e "$identity_path" ] || continue
    [ -w "$identity_path" ] ||
      tng_identity_fail "recursive identity path is not writable as $PUID:$PGID: $identity_path; start without a --user override so the entrypoint can prepare it"
  done
}

tng_identity_exec() {
  if command -v setpriv >/dev/null 2>&1; then
    exec setpriv \
      --reuid="$PUID" \
      --regid="$PGID" \
      --clear-groups \
      --inh-caps=-all \
      --ambient-caps=-all \
      --bounding-set=-all \
      --nnp \
      "$@"
  fi
  if command -v su-exec >/dev/null 2>&1; then
    exec su-exec "$PUID:$PGID" "$@"
  fi
  tng_identity_fail "neither setpriv nor su-exec is available"
}

tng_identity_reap() {
  [ -x "$TNG_INIT" ] || tng_identity_fail "init wrapper is not executable: $TNG_INIT"
  exec "$TNG_INIT" -- "$0" --tng-identity-dropped "$@"
}

tng_identity_enter() {
  tng_identity_validate

  if [ "${1:-}" = --tng-identity-dropped ]; then
    [ "$(id -u)" -ne 0 ] ||
      tng_identity_fail "identity-drop marker cannot be used while running as root"
    tng_identity_check_nonroot
    return
  fi

  if [ "$(id -u)" -eq 0 ]; then
    tng_identity_prepare_paths
    tng_identity_exec "$TNG_INIT" -- "$0" --tng-identity-dropped "$@"
  fi

  tng_identity_reap "$@"
}
