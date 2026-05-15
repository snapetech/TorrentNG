#!/bin/sh
set -e

RTORRENT_SOCKET=/run/rtorrent/rpc.sock
CONFIG_FILE=${RTORRENTNG_CONFIG:-/config/config.toml}

mkdir -p /run/rtorrent /session /data

# Start rTorrent in background
rtorrent -n -o "import=/etc/rtorrent/rtorrent.rc" \
         -o "session.path=/session" \
         -o "scgi_local=$RTORRENT_SOCKET" &

# Wait for socket
for i in $(seq 1 30); do
  [ -S "$RTORRENT_SOCKET" ] && break
  sleep 0.5
done

if [ ! -S "$RTORRENT_SOCKET" ]; then
  echo "rTorrent socket not ready after 15s" >&2
  exit 1
fi

exec rtorrentng --config "$CONFIG_FILE"
