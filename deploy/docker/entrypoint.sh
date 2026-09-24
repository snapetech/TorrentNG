#!/bin/sh
set -e

TNG_INIT=/sbin/tini
TNG_IDENTITY_PATHS="/data /session /var/lib/torrentng /var/log/rtorrent /run/rtorrent"
. /usr/local/lib/torrentng/identity.sh
tng_identity_enter "$@"
if [ "${1:-}" = --tng-identity-dropped ]; then
  shift
fi

RTORRENT_SOCKET=${RTORRENT_SCGI_SOCKET:-/run/rtorrent/rpc.sock}
CONFIG_FILE=${TORRENTNG_CONFIG:-/config/config.toml}
INCOMING_PORT=${RTORRENT_INCOMING_PORT:-50000}
BACKEND=${TNG_BACKEND:-rtorrent}
export TERM="${TERM:-xterm}"
umask 0002

# MANAGE_RTORRENT selects whether this container spawns and owns an rTorrent
# process (the default, matching every existing compose profile) or the
# compatible-client service only talks to an rTorrent instance the operator
# already runs elsewhere (TNG_RTORRENT_MANAGED=0, paired with TNG_SCGI_ADDR or
# TNG_SCGI_SOCKET pointed at that instance). Only meaningful when BACKEND is
# rtorrent; every other backend already skips the local rTorrent process.
MANAGE_RTORRENT=0
if [ "$BACKEND" = "rtorrent" ]; then
  case "${TNG_RTORRENT_MANAGED:-1}" in
    0 | false | no | NO | False) MANAGE_RTORRENT=0 ;;
    *) MANAGE_RTORRENT=1 ;;
  esac
fi

mkdir -p /run/rtorrent /session /data /var/lib/torrentng /var/log/rtorrent /config
rm -f "$RTORRENT_SOCKET" /session/rtorrent.lock

if [ -r /config/rtorrent.rc ]; then
  cp /config/rtorrent.rc /run/rtorrent/user.rc
else
  : > /run/rtorrent/user.rc
fi

if [ "$MANAGE_RTORRENT" = "1" ] && [ -n "${TNG_RTORRENT_OVERLAY:-}" ]; then
  RTORRENT_UI_OVERLAY=$TNG_RTORRENT_OVERLAY
  case "$RTORRENT_UI_OVERLAY" in
    /*) ;;
    *)
      echo "TNG_RTORRENT_OVERLAY must be an absolute path" >&2
      exit 1
      ;;
  esac
  case "$RTORRENT_UI_OVERLAY" in
    *[!A-Za-z0-9_./-]*)
      echo "TNG_RTORRENT_OVERLAY contains unsupported path characters" >&2
      exit 1
      ;;
  esac
  if [ "${#RTORRENT_UI_OVERLAY}" -gt 4096 ]; then
    echo "TNG_RTORRENT_OVERLAY exceeds the path length limit" >&2
    exit 1
  fi
  if [ -L "$RTORRENT_UI_OVERLAY" ]; then
    echo "TNG_RTORRENT_OVERLAY must not be a symlink" >&2
    exit 1
  elif [ ! -e "$RTORRENT_UI_OVERLAY" ]; then
    if ! (umask 0077; set -C; : > "$RTORRENT_UI_OVERLAY") 2>/dev/null; then
      if [ ! -f "$RTORRENT_UI_OVERLAY" ] || [ -L "$RTORRENT_UI_OVERLAY" ]; then
        echo "Cannot create TNG_RTORRENT_OVERLAY" >&2
        exit 1
      fi
    fi
  fi
  if [ ! -f "$RTORRENT_UI_OVERLAY" ] || [ -L "$RTORRENT_UI_OVERLAY" ]; then
    echo "TNG_RTORRENT_OVERLAY must be a regular file" >&2
    exit 1
  fi
  if ! grep -Fqx "import = $RTORRENT_UI_OVERLAY" /run/rtorrent/user.rc; then
    printf '\nimport = %s\n' "$RTORRENT_UI_OVERLAY" >> /run/rtorrent/user.rc
  fi
fi

if [ ! -r "$CONFIG_FILE" ]; then
  if [ "$CONFIG_FILE" = /config/config.toml ]; then
    CONFIG_FILE=/etc/torrentng/config.toml
  else
    echo "TorrentNG config is missing or unreadable: $CONFIG_FILE" >&2
    exit 1
  fi
fi

if [ "$MANAGE_RTORRENT" = "1" ]; then
  cd /data

  # Start rTorrent in background
  rtorrent -n -o "import=/etc/rtorrent/rtorrent.rc" \
           -o "system.daemon.set=true" \
           -o "session.path=/session" \
           -o "network.scgi.open_local=$RTORRENT_SOCKET" \
           -o "network.port_range.set=$INCOMING_PORT-$INCOMING_PORT" \
           -o "dht.port.set=$INCOMING_PORT" \
           -o "dht.override_port.set=$INCOMING_PORT" &

  # Wait for socket
  i=1
  while [ "$i" -le 30 ]; do
    [ -S "$RTORRENT_SOCKET" ] && break
    sleep 0.5
    i=$((i + 1))
  done

  if [ ! -S "$RTORRENT_SOCKET" ]; then
    echo "rTorrent socket not ready after 15s" >&2
    exit 1
  fi

  chmod 660 "$RTORRENT_SOCKET"

  # A socket existing only means that rTorrent has created its SCGI listener;
  # it may still be replaying a large session. Do not let the compatible-client service submit
  # identity RPCs into that startup window. This installation currently needs
  # about 130 seconds to replay 22k session entries; keep a margin for slower
  # storage conditions and allow an operator override when the session size
  # changes.
  startup_grace_secs=${RTORRENT_STARTUP_GRACE_SECS:-0}
  case "$startup_grace_secs" in
    ''|*[!0-9]*)
      echo "RTORRENT_STARTUP_GRACE_SECS must be a non-negative integer" >&2
      exit 1
      ;;
  esac
  if [ "$startup_grace_secs" -gt 0 ]; then
    echo "Waiting ${startup_grace_secs}s for rTorrent session replay to finish..." >&2
    sleep "$startup_grace_secs"
  fi
elif [ "$BACKEND" = "rtorrent" ]; then
  echo "Starting TorrentNG compatible-client service against an external rTorrent instance (TNG_RTORRENT_MANAGED=0)"
else
  echo "Starting TorrentNG compatible-client service with external backend: $BACKEND"
fi

exec torrentng "$CONFIG_FILE"
