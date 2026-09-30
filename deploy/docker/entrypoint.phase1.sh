#!/bin/sh
set -e

TNG_INIT=/sbin/tini
TNG_IDENTITY_PATHS="/data /session /run/rtorrent /var/log/rtorrent /run/nginx /run/php-fpm83 /var/log/nginx /var/log/php83 /var/lib/nginx /var/lib/php83 /var/www/rutorrent/share"
TNG_IDENTITY_RECURSIVE_PATHS="/var/lib/nginx /var/lib/php83 /var/www/rutorrent/share"
. /usr/local/lib/torrentng/identity.sh
tng_identity_enter "$@"
if [ "${1:-}" = --tng-identity-dropped ]; then
    shift
fi

SOCKET=${RTORRENT_SCGI_SOCKET:-/run/rtorrent/rpc.sock}
INCOMING_PORT=${RTORRENT_INCOMING_PORT:-50000}
export TERM="${TERM:-xterm}"
umask 0002
mkdir -p /run/rtorrent /session /data /var/log/rtorrent
rm -f "$SOCKET" /session/rtorrent.lock

# Apply user config overlay if present
if [ -r /config/rtorrent.rc ]; then
    cp /config/rtorrent.rc /run/rtorrent/user.rc
else
    : > /run/rtorrent/user.rc
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
attempt=0
while [ "$attempt" -lt 60 ] && [ ! -S "$SOCKET" ]; do
    sleep 0.5
    attempt=$((attempt + 1))
done

if [ ! -S "$SOCKET" ]; then
    echo "ERROR: rTorrent socket not ready after 30s" >&2
    exit 1
fi

# Set socket group-readable for nginx/PHP-FPM
chmod 660 "$SOCKET"

echo "TorrentNG Phase 1 ready. ruTorrent: http://localhost/"
echo "Socket: $SOCKET"

while pgrep rtorrent >/dev/null && pgrep nginx >/dev/null && pgrep php-fpm83 >/dev/null; do
    sleep 5
done

echo "ERROR: Phase 1 process exited" >&2
exit 1
