#!/bin/sh
set -e

SOCKET=/run/rtorrent/rpc.sock
mkdir -p /run/rtorrent /session /data /var/log/rtorrent

# Apply user config overlay if present
if [ -f /config/rtorrent.rc ]; then
    cp /config/rtorrent.rc /etc/rtorrent/user.rc
fi

# Start PHP-FPM for ruTorrent
php-fpm83 -D

# Start nginx
nginx -g 'daemon on;'

# Start rTorrent in background
rtorrent -n \
    -o "import=/etc/rtorrent/rtorrent.rc" \
    -o "session.path=/session" \
    -o "scgi_local=$SOCKET" &

RT_PID=$!

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

wait $RT_PID
