# Architecture

## Overview

TorrentNG is one product with a shared WebUI and API surface. It can use an
existing compatible torrent client, or it can run the first-party TorrentNG
client. The selected client is the component that performs peer, tracker, and
payload work and owns authoritative transfer state.

- **Compatible-client integration:** the `torrentng` service connects the
  shared WebUI/API to one supported client: rTorrent, qBittorrent, Transmission,
  Deluge, or a separate `torrentngd` process. It translates the common surface
  and maintains a cache; the selected client remains authoritative.
- **TorrentNG client:** `torrentngd` is TorrentNG's first-party, next-generation
  client daemon. It owns torrent state, SQLite session persistence, tracker
  state, peer-wire tasks, storage, rechecks, jobs, metrics, and the API
  projections, and serves the shared WebUI/API directly.

The compatibility promise is capability-aware rather than a claim of identical
feature parity: TorrentNG REST/SSE, qBittorrent Web API shapes, Transmission RPC,
Deluge JSON-RPC, rTorrent integration, multi-client importers, live client
certification, and wire-level transfer matrices define the supported contract.

The source directory `sidecar/` and identifiers such as `sidecar_started` and
`track1_sidecar_required` are retained as repository/API compatibility
contracts. They describe implementation history, not a third product.

For the practical comparison and migration workflow, see
[ENGINE_REWRITE.md](ENGINE_REWRITE.md).

```text
┌─────────────────────────────────────────────────────────┐
│                      External Tools                      │
│   Prowlarr · Sonarr · Radarr · autobrr · cross-seed     │
│              NZB360 · Transdrone · etc.                  │
└────────────────────────┬────────────────────────────────┘
                         │
┌────────────────────────▼────────────────────────────────┐
│              TorrentNG WebUI + API surface               │
│  React/Vite · TorrentNG REST/SSE · qBit · Transmission  │
│                    · Deluge compatibility                │
└────────────────────────┬────────────────────────────────┘
                         │
              ┌──────────┴──────────┐
              │                     │
┌─────────────▼─────────────┐ ┌─────▼─────────────────────┐
│ TorrentNG client           │ │ Compatible-client service │
│ `torrentngd`               │ │ `torrentng`               │
│ owns transfer + state      │ │ cache + adapters           │
│ peer/tracker/storage/jobs  │ │                            │
└───────────────────────────┘ └─────────────┬──────────────┘
                                            │
                         ┌──────────────────┼──────────────────┐
                         │                  │                  │
                      rTorrent         qBittorrent       Transmission/Deluge
```

## TorrentNG Client (`torrentngd`)

**Location:** `crates/`
**Binary:** `crates/torrentngd`

`torrentngd` wires the TorrentNG transfer engine crates into one process.
SQLite-backed client state is the source of truth for torrent rows, file
metadata, trackers, labels, jobs, metrics, and compatibility projections.

### Core Crates

- `rt-metainfo` parses v1, v2, hybrid torrents, and `btih`/`btmh` magnets.
- `rt-db` stores durable torrents, file rows, tracker rows, limits, jobs,
  events, settings, storage roots, mounts, tags, and categories.
- `rt-engine` supervises torrent tasks, metadata placeholders, rechecks,
  tracker state, DHT registration, shutdown, diagnostics, and restore.
- `rt-storage` provides root/mount abstractions, dry-run import/move/delete
  plans, scheduling, and v1/v2 verification.
- `rt-tracker`, `rt-peer-wire`, `rt-peer-manager`, `rt-piece-picker`,
  `rt-dht`, and `rt-utp` cover protocol behavior and peer/download mechanics.
- `rt-api-native`, `rt-api-qbit`, `rt-api-transmission`, and `rt-api-deluge`
  expose the TorrentNG and compatibility APIs over the same registry.
- `rt-migrate` imports rTorrent, qBittorrent, Transmission, Deluge,
  uTorrent/BitTorrent Classic, BiglyBT/Vuze, Tixati, and generic torrent
  library state.
- `rt-metrics` and `rt-testkit` provide scale and certification evidence.

### TorrentNG Client Data Flow

```text
add torrent/magnet
      │
      ▼
parse metainfo or magnet identity
      │
      ▼
persist torrent row, metadata, labels, trackers, and event
      │
      ▼
spawn v1/hybrid torrent task or taskless pure-v2 metadata projection
      │
      ├── tracker manager persists announce/scrape state
      ├── peer tasks verify pieces before completion
      ├── storage scheduler throttles reads, writes, and rechecks
      └── APIs project registry + metadata + fastresume state
```

Startup restores persisted torrents from the DB and metadata store. Pure v2
rows restore as taskless metadata projections when there is no v1 peer-wire
task to spawn.

## Shared Runtime API Layer

The API layer is intentionally a projection over the selected client's state,
not the internal model. The same surface can be served directly by
`torrentngd` or by the compatible-client integration service:

- TorrentNG REST/SSE is snake_case and built for the WebUI and direct integrations.
- qBittorrent v2 compatibility preserves ecosystem behavior for automation.
- Transmission RPC supports session, torrent, tracker, file, queue, and magnet
  surfaces; v2 hashes project as BEP 52 `btmh` magnet links.
- Deluge RPC is a compatibility facade over the same registry and metadata,
  with current parity tracked in the client compatibility matrix.

`GET /health` is the runtime contract for readiness and capability discovery.
When served directly by `torrentngd`, it reports
`engine.track1_sidecar_required=false` plus a machine-readable capability
manifest for v1/v2/hybrid identity, storage safety, jobs, migration, DHT/uTP
policy, compatibility facades, metrics, and diagnostics. The integration
service reports the selected client's backend capabilities and reachability.

## Compatible-client Integration Service (`torrentng`)

**Location:** `sidecar/`
**Binary:** `torrentng`

This service is the WebUI/API host for deployments that keep an existing
client. It selects one backend adapter, translates the common surface, caches
backend state, and serves the WebUI plus compatibility APIs. It does not
perform BitTorrent transfers itself.

Supported integrations are rTorrent, qBittorrent, Transmission, Deluge, and a
separate `torrentngd` process. A direct `torrentngd` deployment is preferred
when TorrentNG should own transfer, storage, durable persistence, rechecks, and
jobs; the integration service is preferred when an existing client or library
must stay in place.

### Responsibilities

- Maintain a live torrent state cache synchronized through the selected client
  adapter.
- Serve TorrentNG REST and qBittorrent/Transmission/Deluge-compatible APIs.
- Serve the shared WebUI, WebSocket events, and Prometheus metrics.
- Enforce auth and script workflow policy.
- Provide common metadata, labels, tracker views, workflows, and migration
  support where the selected client exposes the required capability.

### rTorrent Integration Data Flow

```text
rTorrent ── XMLRPC poll ──► torrentng SQLite cache
                                │
                    ┌───────────┤
                    │           │
              REST clients   WebSocket
```

For rTorrent, the integration service is the only trusted XMLRPC client.
Browser, automation, and scripts talk to TorrentNG, never directly to the SCGI
socket. The same ownership pattern applies to the HTTP/RPC adapters for the
other compatible clients.

## WebUI

**Location:** `webui/`
**Stack:** React 19, TypeScript strict, Vite, TanStack Query, TanStack Virtual,
TanStack Table.

The WebUI is shared by the TorrentNG client and compatible-client integrations.
It is built around large libraries: virtualized rows, server-side
filter/sort/page, delta events, bulk dry-run previews, storage/tracker views,
saved views, and diagnostic actions. Backend capabilities are surfaced to the
UI so unsupported operations can be disabled or reported explicitly.

## Deployment

**Location:** `deploy/`

TorrentNG client deployments run `torrentngd` with durable DB/metadata paths
and storage roots. Compatible-client deployments run `torrentng` with the
selected client, either in the Phase 1 rTorrent bundle or against an existing
hosted rTorrent, qBittorrent, Transmission, or Deluge instance.

The release evidence is split the same way:

- `scripts/native_engine_certification_report.sh` certifies the TorrentNG
  client and can assert a live `/health` capability manifest.
- `scripts/pre_engine_certification_suite.sh` and
  `scripts/pre_engine_release_report.sh` aggregate legacy compatibility,
  integration, security, soak, and TorrentNG-client evidence.
