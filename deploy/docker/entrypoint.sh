#!/bin/sh
set -e

RTORRENT_SOCKET=${RTORRENT_SCGI_SOCKET:-/run/rtorrent/rpc.sock}
CONFIG_FILE=${TORRENTNG_CONFIG:-${RTORRENTNG_CONFIG:-/config/config.toml}}
INCOMING_PORT=${RTORRENT_INCOMING_PORT:-50000}
BACKEND=${TNG_BACKEND:-rtorrent}
export TERM="${TERM:-xterm}"

mkdir -p /run/rtorrent /session /data /var/lib/torrentng /var/log/rtorrent /config
rm -f "$RTORRENT_SOCKET" /session/rtorrent.lock

if [ -f /config/rtorrent.rc ]; then
  cp /config/rtorrent.rc /etc/rtorrent/user.rc
else
  : > /etc/rtorrent/user.rc
fi

if [ ! -f "$CONFIG_FILE" ]; then
  cp /etc/torrentng/config.toml "$CONFIG_FILE"
fi

if [ "$BACKEND" = "rtorrent" ]; then
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
else
  echo "Starting TorrentNG compatible-client service with external backend: $BACKEND"
fi

exec torrentng "$CONFIG_FILE"
