#!/bin/sh
set -e

SOCKET=${RTORRENT_SCGI_SOCKET:-/run/rtorrent/rpc.sock}
INCOMING_PORT=${RTORRENT_INCOMING_PORT:-50000}
export TERM="${TERM:-xterm}"
mkdir -p /run/rtorrent /session /data /var/log/rtorrent
rm -f "$SOCKET" /session/rtorrent.lock

# Apply user config overlay if present
if [ -f /config/rtorrent.rc ]; then
    cp /config/rtorrent.rc /etc/rtorrent/user.rc
else
    : > /etc/rtorrent/user.rc
fi

# Start PHP-FPM for ruTorrent
php-fpm83 -D

# Start nginx
nginx -g 'daemon on;'

# Start rTorrent in background
rtorrent -n \
    -o "import=/etc/rtorrent/rtorrent.rc" \
    -o "system.daemon.set=true" \
    -o "session.path=/session" \
    -o "network.scgi.open_local=$SOCKET" \
    -o "network.port_range.set=$INCOMING_PORT-$INCOMING_PORT" \
    -o "dht.port.set=$INCOMING_PORT" \
    -o "dht.override_port.set=$INCOMING_PORT" &

# Wait for socket (up to 30s)
for i in $(seq 1 60); do
    [ -S "$SOCKET" ] && break
    sleep 0.5
done

if [ ! -S "$SOCKET" ]; then
    echo "ERROR: rTorrent socket not ready after 30s" >&2
    exit 1
fi

# Set socket group-readable for nginx/PHP-FPM
chmod 660 "$SOCKET"

echo "rtorrentNG Phase 1 ready. ruTorrent: http://localhost/"
echo "Socket: $SOCKET"

while pgrep rtorrent >/dev/null && pgrep nginx >/dev/null && pgrep php-fpm83 >/dev/null; do
    sleep 5
done

echo "ERROR: Phase 1 process exited" >&2
exit 1
