# rTorrent tinyxml2 Build Profile

This directory contains the Phase 1 build helper for a pinned rTorrent engine.

Defaults:

| Variable | Default |
|---|---|
| `RTORRENT_REF` | `v0.16.11` |
| `LIBTORRENT_REF` | `v0.16.11` |
| `PREFIX` | `/usr/local` |
| `WORKDIR` | `/tmp/torrentng-build` |

Run on a host with build dependencies installed:

```sh
./engine-profile/build/build-rtorrent-tinyxml2.sh
```

The Phase 1 Dockerfile uses the same flags:

```sh
./engine-profile/build/build-rtorrent-tinyxml2.sh
```

Important configure flag:

```sh
--with-xmlrpc-tinyxml2
```

That selects tinyxml2 XMLRPC handling for the known-good bundle. The host helper mirrors the Docker build flow: run `autoreconf -fi`, build matching rTorrent/libtorrent tags, and apply the TorrentNG user-agent XMLRPC patch when available.
