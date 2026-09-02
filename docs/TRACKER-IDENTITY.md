# Tracker Identity

TorrentNG pins one tracker-facing rTorrent 0.16.11 identity **family** for
both the native engine and the rTorrent sidecar:

```toml
user_agent = "rtorrent/0.16.11/0.16.11"
peer_id_prefix = "-lt100B-"
```

The peer ID is exactly 20 ASCII bytes: the 8-byte prefix above plus a
**12-byte suffix that must be unique per install**. Since 2026-09,
that suffix is generated once and persisted on first start (see
"Per-install peer ID generation" below) — it is no longer a hardcoded
literal. `user_agent` has no such requirement; sharing it across installs is
fine and expected.

## Incident: shared peer ID suffix caused MAM multi-client bans (2026-09-02)

From first release through 2026-09, the suffix was the hardcoded literal
`000000000000`, i.e. every install that did not explicitly set
`TNG_PEER_ID`/`TORRENTNG_PEER_ID` presented the byte-identical peer ID
`-lt100B-000000000000`. A user running TorrentNG against MAM (MyAnonamouse)
was banned three times for "running 10+ instances of the same client" while
seeding from a single host.

Root cause: peer_id exists specifically so a tracker can tell client
instances apart. Private-tracker anti-cheat (MAM's client/peer verification
included) treats the same peer_id showing up across multiple swarm
connections as one operator running several simultaneous clients — a
violation of most private trackers' multi-client rules. Because the suffix
was a compile-time literal shared by every unconfigured TorrentNG
deployment on earth (confirmed in every shipped default: `crates/rt-engine/
src/peer_id.rs`, `sidecar/src/config.rs`, and the packaged
`deploy/docker/sidecar.config.toml`, `deploy/systemd/torrentng.config.toml`,
and both `compose.yml` files' `TNG_PEER_ID` default), any two independent
installs seeding the same swarm — including two of the *same* user's own
hosts — would present the identical id, which is exactly what tracker
multi-client detection looks for.

This was not a rate-limiting, port-reuse, DHT, or dual-stack (native +
sidecar) issue — those were checked and ruled out. It was purely the static
peer_id suffix. Fixed by generating a random 12-byte suffix per install and
persisting it (`crates/rt-engine/src/peer_id.rs::init`,
`sidecar/src/identity.rs::load_or_generate_peer_id`), so every install
still presents the `-lt100B-` family prefix (needed for trackers that
whitelist known clients) but a suffix unique to that one install.

**Do not revert the suffix to a shared/hardcoded value.** If you need a
reproducible id for testing, set `TNG_PEER_ID`/`TORRENTNG_PEER_ID`
explicitly rather than changing the generator's default.

**If you were affected by this before upgrading:** the new peer id is
generated fresh on first start after upgrade (see "Migrating an existing
install" below). Since it differs from the historical shared literal, it is
a *new* identity from the tracker's point of view — some trackers may still
want confirmation from support that a client bug, not intentional
multi-clienting, produced the earlier flag before removing a standing ban.

## Source Of Truth

This pair is derived from upstream rTorrent/libtorrent 0.16.11 source:

- libtorrent `configure.ac` sets `PEER_NAME` to `-lt100B-` for 0.16.11.
- rTorrent `configure.ac` builds `USER_AGENT` as `PACKAGE/VERSION/` plus
  `torrent::version()`, which is `rtorrent/0.16.11/0.16.11` when linked with
  libtorrent 0.16.11.

Both halves matter. Private trackers can reject a client when the peer ID family
and HTTP User-Agent do not match the client/version pair they allow.

## Per-install peer ID generation

Both engines resolve the peer_id the same way, in priority order:

1. `TORRENTNG_PEER_ID` / `TNG_PEER_ID` (native) or `TNG_PEER_ID` /
   `RTNG_PEER_ID` (sidecar) — a full, explicit 20-byte override. Always wins.
2. A 12-byte suffix persisted from a previous run:
   - Native: `<session_dir>/peer_id_suffix`
     (`crates/rt-engine/src/peer_id.rs`).
   - Sidecar: `<data_dir>/peer_id_suffix` (`sidecar/src/identity.rs`).
3. Otherwise, a freshly generated random 12-character alphanumeric suffix,
   written to that same file so future restarts stay on the same identity.

The sidecar additionally treats `rtorrent.peer_id` in `config.toml` as a
sentinel: if it is still exactly `-lt100B-000000000000` after config-file and
env overrides are applied (i.e. nobody actually customized it), `Config::load`
resolves and persists a real per-install id in its place before startup
continues. This makes upgrading self-healing — an old config file or deploy
template that still hardcodes the literal default does not defeat the fix.

## Native Engine

`crates/rt-engine/src/peer_id.rs` exposes `peer_id::init(session_dir)`,
called once from `torrentngd`'s `main.rs` right after the session directory
is created and before the engine starts. `our_peer_id()` (used by tracker
and peer-wire code) reads the value `init` resolved.

Explicit overrides:

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

Only set `TNG_PEER_ID`/`TORRENTNG_PEER_ID` to a literal like the example
above for reproducible testing on a single, throwaway install. Never bake a
literal peer_id into a shared config template, image, or fleet-wide env
var — that recreates the incident above.

## rTorrent Sidecar

The sidecar uses the same `user_agent` in `sidecar/src/config.rs` and the
packaged deployment configs; `peer_id` is resolved per-install as described
above (packaged configs no longer set it):

```toml
[rtorrent]
user_agent = "rtorrent/0.16.11/0.16.11"
# peer_id intentionally omitted — see "Per-install peer ID generation"
```

Explicit override (only for pinning a specific install, not for templates):

```sh
TNG_USER_AGENT="rtorrent/0.16.11/0.16.11"
TNG_PEER_ID="-lt100B-000000000000"
```

The sidecar also accepts `RTNG_USER_AGENT` and `RTNG_PEER_ID` for older host
service files. Those aliases must use the same values.

On startup the sidecar:

1. Calls `network.http.user_agent.set` with rTorrent's required leading empty
   XMLRPC target argument.
2. Calls `d.multicall2` with `d.local_id.set=<resolved peer_id>` for loaded
   downloads.
3. Calls `d.save_full_session=` so the local IDs survive restart.

The leading empty XMLRPC argument is the rTorrent target slot. It is not a
sidecar identity and must not be removed.

## Migrating an existing install

Existing installs self-heal on their next restart after upgrading — no
manual file edits are required:

- **Native (`torrentngd`)**: on next start, `peer_id::init` finds no
  `<session_dir>/peer_id_suffix`, generates one, and persists it.
- **Sidecar**: on next start, `Config::load` sees `rtorrent.peer_id` is
  still the sentinel `-lt100B-000000000000` (whether from an old packaged
  config file or from having never set it) and replaces it with a generated,
  persisted id — unless the config or `TNG_PEER_ID`/`RTNG_PEER_ID` was
  explicitly set to something else, which is left untouched.

To confirm the new identity took effect, see Verification below. If a
tracker banned the account before this fix while `d.local_id` matched the
shared literal (see Incident above), restarting is necessary but may not be
sufficient — some trackers require a support ticket confirming the cause
before lifting the ban, since the old and new peer_ids are different
identities from the tracker's point of view.

## Values Not To Reintroduce

Do not use these:

| Bad value | Why it is wrong |
|---|---|
| `user_agent = "rtorrent/0.16.11"` | Strips the linked libtorrent version from rTorrent's upstream User-Agent. |
| `peer_id = "rtorrent/0.16.11/000"` | Starts with `rto`; trackers parse it as the wrong client family. |
| `peer_id = "-lt1011-000000000000"` | Guessed decimal version encoding; upstream libtorrent 0.16.11 uses `-lt100B-`. |
| Any hardcoded/shared 12-byte peer_id suffix, in code, a config template, or a fleet-wide env var | Recreates the 2026-09 MAM multi-client incident above — the suffix must be generated per install, never shared. |
| `libtorrent-0.16.11-rtorrent-peer-name.patch` | Wrong fix path; do not patch libtorrent's upstream peer name for this. |

## Verification

For a packaged rTorrent sidecar container, verify the live tracker identity with
an SCGI/XMLRPC helper when one is available:

```sh
xmlrpc localhost network.http.user_agent
xmlrpc localhost d.multicall.range '' main 0 5 d.hash= d.local_id=
```

Expected: `network.http.user_agent` is `rtorrent/0.16.11/0.16.11`, and
`d.local_id` (hex) starts with the prefix bytes for `-lt100B-`
(`2D6C74313030422D`) followed by 12 bytes that are **not** all `30`
(ASCII `0`) — an all-`30` suffix means resolution fell back to the historical
sentinel and something is wrong (check `data_dir`/`session_dir` is writable).

The stock production image is intentionally small and may not ship an `xmlrpc`
CLI. The sidecar API can still verify the applied User-Agent when authenticated:

```sh
curl -H "Authorization: Bearer $TNG_API_TOKEN" \
  http://127.0.0.1:8080/api/v1/settings/user-agent
```

To confirm the persisted per-install suffix directly:

```sh
# sidecar
cat "$TNG_DATA_DIR/peer_id_suffix"     # e.g. /var/lib/torrentng/peer_id_suffix
# native
cat "$TORRENTNGD_SESSION_DIR/peer_id_suffix"
```

It should be 12 alphanumeric characters and must differ between any two
independent installs. The image should also contain the upstream peer
prefix:

```sh
strings /usr/local/lib/libtorrent.so | grep -E -- '-lt100B-|-lt1011-'
printenv TNG_USER_AGENT RTNG_USER_AGENT
```

Expected result: `-lt100B-` is present, `-lt1011-` is absent, and any
`TNG_USER_AGENT` or `RTNG_USER_AGENT` override is
`rtorrent/0.16.11/0.16.11`.
