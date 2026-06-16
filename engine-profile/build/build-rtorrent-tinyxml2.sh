#!/usr/bin/env sh
set -eu

PREFIX=${PREFIX:-/usr/local}
WORKDIR=${WORKDIR:-/tmp/torrentng-build}
LIBTORRENT_REF=${LIBTORRENT_REF:-v0.16.11}
RTORRENT_REF=${RTORRENT_REF:-v0.16.11}
JOBS=${JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)}
PATCH_FILE=${PATCH_FILE:-$(pwd)/deploy/docker/patches/rtorrent-0.16.11-user-agent-command.patch}
RTORRENT_LOCAL_ID_PATCH_FILE=${RTORRENT_LOCAL_ID_PATCH_FILE:-$(pwd)/deploy/docker/patches/rtorrent-0.16.11-local-id-command.patch}

mkdir -p "$WORKDIR"
cd "$WORKDIR"

if [ ! -d libtorrent ]; then
  git clone --depth=1 --branch "$LIBTORRENT_REF" https://github.com/rakshasa/libtorrent.git
fi
if [ ! -d rtorrent ]; then
  git clone --depth=1 --branch "$RTORRENT_REF" https://github.com/rakshasa/rtorrent.git
fi
if [ -f "$PATCH_FILE" ] && ! git -C "$WORKDIR/rtorrent" grep -q 'network.http.user_agent'; then
  git -C "$WORKDIR/rtorrent" apply "$PATCH_FILE"
fi
if [ -f "$RTORRENT_LOCAL_ID_PATCH_FILE" ] && ! git -C "$WORKDIR/rtorrent" grep -q 'd.local_id.set'; then
  git -C "$WORKDIR/rtorrent" apply "$RTORRENT_LOCAL_ID_PATCH_FILE"
fi

cd "$WORKDIR/libtorrent"
autoreconf -fi
./configure --prefix="$PREFIX"
make -j"$JOBS"
make install

cd "$WORKDIR/rtorrent"
autoreconf -fi
./configure --prefix="$PREFIX" --with-xmlrpc-tinyxml2
make -j"$JOBS"
make install

"$PREFIX/bin/rtorrent" -h | head -n 1
