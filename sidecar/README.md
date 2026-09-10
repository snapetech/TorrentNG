# sidecar/

The Track 1 rTorrent-backed daemon — the compatibility and migration bridge
described in [docs/ENGINE_REWRITE.md](../docs/ENGINE_REWRITE.md), not the
primary engine. If you're looking for the native backend, that's
`torrentngd` in [crates/torrentngd/](../crates/torrentngd/).

## What this is

`torrentng` (the binary built here) runs beside an existing rTorrent process
and talks to it over a trusted local SCGI/XMLRPC socket. It does not run its
own BitTorrent engine — rTorrent/libtorrent owns peer traffic, piece storage,
and session state. The sidecar is the control plane in front of it: auth, a
local cache, WebUI serving, native REST, and the qBittorrent-compatible API
that Sonarr, Radarr, Prowlarr, autobrr, and cross-seed expect.

Use this when you:

- are migrating off rTorrent gradually and want to keep running it a while
  longer under a modern WebUI/API layer,
- want a side-by-side comparison against the native engine, or
- specifically want to keep upstream rTorrent/libtorrent as your BitTorrent
  core.

If none of those apply, run the native engine instead
(`torrentngd`, [crates/torrentngd/](../crates/torrentngd/)) — it's the
project's primary, actively developed backend and needs neither this sidecar
nor rTorrent at all.

## Why a separate Cargo workspace

`sidecar/` has its own `Cargo.toml`/`Cargo.lock` and is intentionally outside
the root workspace under `crates/`. Track 1 exists for compatibility and
migration, not for new engine capability, so it evolves on its own cadence
without tracking the native engine crates' churn.

## Layout

| Module | Purpose |
|---|---|
| `config` | TOML config loading, `TNG_*` env overrides |
| `rtorrent::client` | Async XMLRPC/SCGI client over Unix socket or TCP |
| `rtorrent::torrents` | `d.multicall2` torrent queries, CRUD ops, user-agent push |
| `api::server` / `api::handlers` | axum router and native REST handlers |
| `api::ws` | WebSocket event broadcast |
| `qbcompat` | qBittorrent v2 API shim |
| `cache::db` / `cache::query` | rusqlite cache schema and server-side filter/sort/paginate |
| `sync` | Background poll -> cache upsert -> WS broadcast loop |

## Build and run

```sh
cd sidecar
cargo build
cargo test
```

Or via Docker Compose (rTorrent + sidecar together):

```sh
docker compose -f ../deploy/docker/compose.yml up --build
```

## Related docs

- [docs/ENGINE_REWRITE.md](../docs/ENGINE_REWRITE.md) — native vs. sidecar,
  side by side
- [docs/DEPLOYMENT.md](../docs/DEPLOYMENT.md) — Track 1 deployment
- [docs/MIGRATION.md](../docs/MIGRATION.md) — moving state in or out, in
  either direction
- [docs/TRACKER-IDENTITY.md](../docs/TRACKER-IDENTITY.md) — user-agent/peer-id
  policy shared by both engines
