# Tracker Identity

TorrentNG pins one tracker-facing rTorrent 0.16.11 identity pair for both the
native engine defaults and the rTorrent sidecar defaults:

```toml
user_agent = "rtorrent/0.16.11/0.16.11"
peer_id = "-lt100B-000000000000"
```

The peer ID is exactly 20 ASCII bytes. rTorrent exposes it as `d.local_id`; the
XMLRPC read command returns hex, so the expected hex value is:

```text
2D6C74313030422D303030303030303030303030
```

## Source Of Truth

This pair is derived from upstream rTorrent/libtorrent 0.16.11 source:

- libtorrent `configure.ac` sets `PEER_NAME` to `-lt100B-` for 0.16.11.
- rTorrent `configure.ac` builds `USER_AGENT` as `PACKAGE/VERSION/` plus
  `torrent::version()`, which is `rtorrent/0.16.11/0.16.11` when linked with
  libtorrent 0.16.11.

Both halves matter. Private trackers can reject a client when the peer ID family
and HTTP User-Agent do not match the client/version pair they allow.

## Native Engine

The native engine uses the same pair in `crates/rt-engine/src/peer_id.rs`.

Accepted overrides:

```sh
TNG_USER_AGENT="rtorrent/0.16.11/0.16.11" \
TNG_PEER_ID="-lt100B-000000000000" \
torrentngd
```

Legacy override names also exist for native-only compatibility:

```sh
TORRENTNG_USER_AGENT="rtorrent/0.16.11/0.16.11"
TORRENTNG_PEER_ID="-lt100B-000000000000"
```

## rTorrent Sidecar

The sidecar uses the same pair in `sidecar/src/config.rs` and in the packaged
deployment configs:

```toml
[rtorrent]
user_agent = "rtorrent/0.16.11/0.16.11"
peer_id = "-lt100B-000000000000"
```

Accepted environment overrides:

```sh
TNG_USER_AGENT="rtorrent/0.16.11/0.16.11"
TNG_PEER_ID="-lt100B-000000000000"
```

The sidecar also accepts `RTNG_USER_AGENT` and `RTNG_PEER_ID` for older host
service files. Those aliases must use the same values.

On startup the sidecar:

1. Calls `network.http.user_agent.set` with rTorrent's required leading empty
   XMLRPC target argument.
2. Calls `d.multicall2` with `d.local_id.set=-lt100B-000000000000` for loaded
   downloads.
3. Calls `d.save_full_session=` so the local IDs survive restart.

The leading empty XMLRPC argument is the rTorrent target slot. It is not a
sidecar identity and must not be removed.

## Values Not To Reintroduce

Do not use these:

| Bad value | Why it is wrong |
|---|---|
| `user_agent = "rtorrent/0.16.11"` | Strips the linked libtorrent version from rTorrent's upstream User-Agent. |
| `peer_id = "rtorrent/0.16.11/000"` | Starts with `rto`; trackers parse it as the wrong client family. |
| `peer_id = "-lt1011-000000000000"` | Guessed decimal version encoding; upstream libtorrent 0.16.11 uses `-lt100B-`. |
| `libtorrent-0.16.11-rtorrent-peer-name.patch` | Wrong fix path; do not patch libtorrent's upstream peer name for this. |

## Verification

For a packaged rTorrent sidecar container, verify the live tracker identity with
an SCGI/XMLRPC helper when one is available:

```sh
xmlrpc localhost network.http.user_agent
xmlrpc localhost d.multicall.range '' main 0 5 d.hash= d.local_id=
```

Expected values:

```text
network.http.user_agent = rtorrent/0.16.11/0.16.11
d.local_id = 2D6C74313030422D303030303030303030303030
```

The stock production image is intentionally small and may not ship an `xmlrpc`
CLI. The sidecar API can still verify the applied User-Agent when authenticated:

```sh
curl -H "Authorization: Bearer $TNG_API_TOKEN" \
  http://127.0.0.1:8080/api/v1/settings/user-agent
```

The image should also contain the upstream peer prefix and the packaged runtime
default:

```sh
strings /usr/local/lib/libtorrent.so | grep -E -- '-lt100B-|-lt1011-'
printenv TNG_USER_AGENT RTNG_USER_AGENT
```

Expected result: `-lt100B-` is present, `-lt1011-` is absent, and any
`TNG_USER_AGENT` or `RTNG_USER_AGENT` override is
`rtorrent/0.16.11/0.16.11`.
