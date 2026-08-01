#!/bin/sh
set -e

RTORRENT_SOCKET=${RTORRENT_SCGI_SOCKET:-/run/rtorrent/rpc.sock}
CONFIG_FILE=${TORRENTNG_CONFIG:-/config/config.toml}
INCOMING_PORT=${RTORRENT_INCOMING_PORT:-50000}
BACKEND=${TNG_BACKEND:-rtorrent}
export TERM=${TERM:-xterm}

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
  for i in $(seq 1 30); do
    [ -S "$RTORRENT_SOCKET" ] && break
    sleep 0.5
  done

  if [ ! -S "$RTORRENT_SOCKET" ]; then
    echo "rTorrent socket not ready after 15s" >&2
    exit 1
  fi

  chmod 660 "$RTORRENT_SOCKET"
else
  echo "Starting TorrentNG sidecar with external backend: $BACKEND"
fi

exec torrentng "$CONFIG_FILE"
