# TorrentNG WebUI/API service

This directory is the current repository path for TorrentNG's compatible-client
WebUI/API service. The executable built here is `torrentng`: it serves the
shared WebUI, TorrentNG REST API, qBittorrent/Transmission/Deluge compatibility
APIs, cache, workflows, auth, and metrics. It does not perform BitTorrent
transfers itself.

The separate `webui/` directory contains the React frontend. The TorrentNG
client is `torrentngd` in
[crates/torrentngd/](../crates/torrentngd/), which can serve the same WebUI/API
directly while also owning transfer, storage, and durable session state.

## What this is

`torrentng` connects to one selected compatible torrent client. Supported
integrations include rTorrent through a trusted local or remote SCGI/XMLRPC
connection, qBittorrent through its Web API, Transmission through RPC, Deluge
through JSON-RPC, and a separate `torrentngd` through its TorrentNG HTTP API.
The selected client owns peer traffic, payload storage, and session state;
TorrentNG supplies the shared WebUI/API, authentication, cache, workflows,
metrics, and compatibility projections.

Use this when you:

- already have a supported torrent client and want a modern WebUI/API without
  moving its library,
- want one interface for automation tools that speak qBittorrent,
  Transmission, or Deluge APIs, or
- want to compare a compatible client with the next-generation TorrentNG
  client before migrating.

If you want TorrentNG to own transfer, storage, persistence, rechecks, and
durable jobs, run the TorrentNG client directly
(`torrentngd`, [crates/torrentngd/](../crates/torrentngd/)). It is the primary
first-party client and does not require an external torrent client.

## Why a separate Cargo workspace

This directory has its own `Cargo.toml`/`Cargo.lock` and is intentionally
outside the root workspace under `crates/`. It contains the compatible-client
integration service, not the TorrentNG transfer client. Keeping the workspace
separate prevents external-client adapter and WebUI-service changes from
coupling the TorrentNG client build to every upstream API dependency.

## Layout

| Module | Purpose |
|---|---|
| `config` | TOML config loading, `TNG_*` env overrides |
| `rtorrent::client` | Async XMLRPC/SCGI client over Unix socket or TCP |
| `rtorrent::torrents` | `d.multicall2` torrent queries, CRUD ops, user-agent push |
| `api::server` / `api::handlers` | axum router and TorrentNG REST handlers |
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

Or via Docker Compose with the rTorrent-compatible integration:

```sh
docker compose -f ../deploy/docker/compose.yml up --build
```

## Related docs

- [docs/ENGINE_REWRITE.md](../docs/ENGINE_REWRITE.md) — compatible-client
  integrations and the TorrentNG client, side by side
- [docs/DEPLOYMENT.md](../docs/DEPLOYMENT.md) — compatible-client deployment
- [docs/MIGRATION.md](../docs/MIGRATION.md) — moving state in or out, in
  either direction
- [docs/TRACKER-IDENTITY.md](../docs/TRACKER-IDENTITY.md) — user-agent/peer-id
  policy shared by both engines
